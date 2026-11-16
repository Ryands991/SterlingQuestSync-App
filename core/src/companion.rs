//! The companion add-on folder and the backups kept before each change.
//!
//! All writes go through `SafeFs`, so they can only land in `AddOns/SterlingQuestSync` or the
//! app's own data folder. Backups are copies read from WTF; the app never writes back into WTF.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::formats::{render_inbox, Inbox};
use crate::paths;
use crate::safety::{Root, SafeFs};
use crate::{Error, Result};

static RE_ADDON_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]{1,40}\.(lua|toc|xml|tga)$").unwrap());

/// How many backup sets to keep.
pub const KEEP_BACKUPS: usize = 30;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Installs or refreshes the companion add-on's own files (bundled with the app).
/// Inbox.lua is never part of this; it is only written by `write_inbox`.
pub fn install(fs_: &SafeFs, files: &[(&str, &[u8])]) -> Result<()> {
    let dir = fs_.root(Root::Companion).to_path_buf();
    for (name, _) in files {
        if !RE_ADDON_FILE.is_match(name) || name.eq_ignore_ascii_case("Inbox.lua") {
            return Err(Error::Refused(format!("bad companion file name: {name}")));
        }
    }
    fs_.create_dir_all(&dir)?;
    for (name, bytes) in files {
        fs_.write_atomic(&dir.join(name), bytes)?;
    }
    let inbox = dir.join("Inbox.lua");
    if !inbox.exists() {
        fs_.write_atomic(&inbox, crate::formats::empty_inbox_lua().as_bytes())?;
    }
    Ok(())
}

pub fn is_installed(fs_: &SafeFs) -> bool {
    let d = fs_.root(Root::Companion);
    d.join("SterlingQuestSync.toc").is_file() && d.join("Inbox.lua").is_file()
}

/// Writes Inbox.lua (temp file and swap). Skips the write when nothing changed.
pub fn write_inbox(fs_: &SafeFs, inbox: &Inbox, written: i64) -> Result<bool> {
    let text = render_inbox(inbox, written)?;
    let path = fs_.root(Root::Companion).join("Inbox.lua");
    if let Ok(old) = fs::read_to_string(&path) {
        // Ignore the `written` line when comparing, so an unchanged inbox isn't rewritten.
        if strip_written(&old) == strip_written(&text) {
            return Ok(false);
        }
    }
    fs_.write_atomic(&path, text.as_bytes())?;
    Ok(true)
}

fn strip_written(s: &str) -> String {
    s.lines().filter(|l| !l.trim_start().starts_with("written =")).collect::<Vec<_>>().join("\n")
}

fn backups_dir(fs_: &SafeFs) -> PathBuf {
    fs_.root(Root::AppData).join("backups")
}

/// Copies Sterling's and the companion's SavedVariables (and the game's .bak copies) for every
/// WTF account into a new backup set. Returns the set's folder, or None if there was nothing to copy.
pub fn backup_saved_variables(fs_: &SafeFs, reason: &str) -> Result<Option<PathBuf>> {
    let reason: String = reason.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(24).collect();
    let set = backups_dir(fs_).join(format!("{}-{}", now(), if reason.is_empty() { "sync" } else { &reason }));
    let mut copied = 0;
    for acct in paths::wtf_accounts(fs_.game()) {
        let sv = paths::saved_variables(fs_.game(), &acct);
        for name in ["Sterling.lua", "Sterling.lua.bak", "SterlingQuestSync.lua", "SterlingQuestSync.lua.bak"] {
            let src = sv.join(name);
            if src.is_file() {
                fs_.copy_in(&src, &set.join(&acct).join(name))?;
                copied += 1;
            }
        }
    }
    if copied == 0 {
        return Ok(None);
    }
    prune_backups(fs_)?;
    Ok(Some(set))
}

/// Backup sets, newest first.
pub fn list_backups(fs_: &SafeFs) -> Vec<PathBuf> {
    let mut v: Vec<(u64, PathBuf)> = fs::read_dir(backups_dir(fs_))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.split('-').next()?.parse::<u64>().ok().map(|t| (t, e.path()))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    v.into_iter().map(|(_, p)| p).collect()
}

fn prune_backups(fs_: &SafeFs) -> Result<()> {
    for old in list_backups(fs_).into_iter().skip(KEEP_BACKUPS) {
        fs_.remove_scratch(&old)?;
    }
    Ok(())
}

/// "Delete" inside the allowlist: moves into the app's trash folder instead.
pub fn move_to_trash(fs_: &SafeFs, p: &Path) -> Result<PathBuf> {
    let name = p.file_name().ok_or_else(|| Error::Refused("nothing to trash".into()))?;
    let dst = fs_.root(Root::AppData).join("trash").join(format!("{}-{}", now(), name.to_string_lossy()));
    fs_.rename(p, &dst)?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::{InPacket, Kind};

    fn setup() -> (tempfile::TempDir, SafeFs) {
        let t = tempfile::tempdir().unwrap();
        let g = t.path().join("_forever_");
        fs::create_dir_all(g.join("Interface/AddOns/Questie")).unwrap();
        fs::write(g.join("Interface/AddOns/Questie/Questie.toc"), "x").unwrap();
        let sv = g.join("WTF/Account/TESTACCOUNT/SavedVariables");
        fs::create_dir_all(&sv).unwrap();
        fs::write(sv.join("Sterling.lua"), "SterlingDB = {}").unwrap();
        fs::write(sv.join("Sterling.lua.bak"), "SterlingDB = {}").unwrap();
        fs::write(sv.join("Questie.lua"), "QuestieDB = {}").unwrap();
        let s = SafeFs::new(&g, &t.path().join("appdata")).unwrap();
        (t, s)
    }

    #[test]
    fn installs_the_companion_and_an_empty_inbox() {
        let (_t, s) = setup();
        install(&s, &[("SterlingQuestSync.toc", crate::formats::companion_toc().as_bytes()), ("Core.lua", b"-- core")]).unwrap();
        assert!(is_installed(&s));
        assert!(install(&s, &[("../Questie/Questie.toc", b"x")]).is_err());
        assert!(install(&s, &[("Inbox.lua", b"x")]).is_err());
        assert!(install(&s, &[("evil.exe", b"x")]).is_err());
        assert_eq!(fs::read_to_string(s.game().join("Interface/AddOns/Questie/Questie.toc")).unwrap(), "x");
    }

    #[test]
    fn inbox_is_only_rewritten_when_it_changes() {
        let (_t, s) = setup();
        let mut ib = Inbox::default();
        ib.put_packet("Player-6800-0000A11C", InPacket { seq: 1, from: "Player-6800-0A1B2C3D".into(), kind: Kind::Partner, time: 1794770000, proto: 6, data: "AB".into() });
        assert!(write_inbox(&s, &ib, 1794770000).unwrap());
        assert!(!write_inbox(&s, &ib, 1794770999).unwrap());
        ib.put_packet("Player-6800-0000A11C", InPacket { seq: 2, from: "Player-6800-0A1B2C3D".into(), kind: Kind::Partner, time: 1794770100, proto: 6, data: "CD".into() });
        assert!(write_inbox(&s, &ib, 1794771000).unwrap());
    }

    #[test]
    fn backups_copy_from_wtf_and_leave_it_untouched() {
        let (_t, s) = setup();
        let sv = s.game().join("WTF/Account/TESTACCOUNT/SavedVariables");
        let before: Vec<_> = fs::read_dir(&sv).unwrap().flatten().map(|e| e.file_name()).collect();
        let set = backup_saved_variables(&s, "test").unwrap().unwrap();
        assert!(set.join("TESTACCOUNT/Sterling.lua").is_file());
        assert!(set.join("TESTACCOUNT/Sterling.lua.bak").is_file());
        assert!(!set.join("TESTACCOUNT/Questie.lua").exists(), "only Sterling's files");
        let after: Vec<_> = fs::read_dir(&sv).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(before.len(), after.len());
        assert_eq!(fs::read_to_string(sv.join("Sterling.lua")).unwrap(), "SterlingDB = {}");
    }

    #[test]
    fn old_backups_are_pruned() {
        let (_t, s) = setup();
        for i in 0..(KEEP_BACKUPS + 5) {
            fs::create_dir_all(backups_dir(&s).join(format!("{}-old", 1000 + i))).unwrap();
        }
        backup_saved_variables(&s, "x").unwrap();
        assert_eq!(list_backups(&s).len(), KEEP_BACKUPS);
    }

    #[test]
    fn trash_refuses_wtf_and_other_addons() {
        let (_t, s) = setup();
        assert!(move_to_trash(&s, &s.game().join("WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua")).is_err());
        assert!(move_to_trash(&s, &s.game().join("Interface/AddOns/Questie")).is_err());
        install(&s, &[("SterlingQuestSync.toc", b"x")]).unwrap();
        let moved = move_to_trash(&s, &s.root(Root::Companion).join("SterlingQuestSync.toc")).unwrap();
        assert!(moved.is_file());
    }
}
