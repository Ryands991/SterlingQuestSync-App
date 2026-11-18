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

/// Unix time in seconds.
fn now() -> u64
{
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Installs or refreshes the companion add-on's own files (bundled with the app).
/// Inbox.lua is never part of this; it is only written by `write_inbox`.
pub fn install(fs_: &SafeFs, files: &[(&str, &[u8])]) -> Result<()>
{
    let dir = fs_.root(Root::Companion).to_path_buf();

    for (name, _) in files
    {
        if !RE_ADDON_FILE.is_match(name) || name.eq_ignore_ascii_case("Inbox.lua")
        {
            return Err(Error::Refused(format!("bad companion file name: {name}")));
        }
    }

    fs_.create_dir_all(&dir)?;

    for (name, bytes) in files
    {
        fs_.write_atomic(&dir.join(name), bytes)?;
    }

    let inbox = dir.join("Inbox.lua");

    if !inbox.exists()
    {
        fs_.write_atomic(&inbox, crate::formats::empty_inbox_lua().as_bytes())?;
    }

    Ok(())
}

/// True when the game saved Sterling's data after the helper add-on was put in place, but the
/// helper saved nothing with it. Both save at the same moment when both are loaded, so this means
/// WoW didn't load the helper: usually the game was open when it was added (only a full restart
/// finds new add-ons), or it's unticked in the AddOns list. Only reads files.
pub fn helper_not_loaded(game: &Path) -> bool
{
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    let Some(added) = mtime(&paths::companion_dir(game).join("SterlingQuestSync.toc")) else { return false };
    // The account whose Sterling data was saved most recently.
    let latest = paths::wtf_accounts(game).into_iter().filter_map(|a| mtime(&paths::sterling_saved(game, &a)).map(|t| (t, a))).max_by_key(|(t, _)| *t);
    let Some((sterling, acct)) = latest else { return false };

    if sterling <= added + std::time::Duration::from_secs(5)
    {
        return false; // the game hasn't saved since the helper was added
    }

    match mtime(&paths::outbox_file(game, &acct))
    {
        None => true,
        Some(helper) => helper + std::time::Duration::from_secs(120) < sterling,
    }
}

/// Is the companion add-on in the game's AddOns folder?
pub fn is_installed(fs_: &SafeFs) -> bool
{
    let dir = fs_.root(Root::Companion);

    dir.join("SterlingQuestSync.toc").is_file() && dir.join("Inbox.lua").is_file()
}

/// Writes Inbox.lua (temp file and swap). Skips the write when nothing changed.
pub fn write_inbox(fs_: &SafeFs, inbox: &Inbox, written: i64) -> Result<bool>
{
    let text = render_inbox(inbox, written)?;
    let path = fs_.root(Root::Companion).join("Inbox.lua");

    if let Ok(old) = fs::read_to_string(&path)
    {
        // Ignore the `written` line when comparing, so an unchanged inbox isn't rewritten.
        if strip_written(&old) == strip_written(&text)
        {
            return Ok(false);
        }
    }

    fs_.write_atomic(&path, text.as_bytes())?;
    Ok(true)
}

/// The text without its "written =" line, to compare two inboxes by content.
fn strip_written(text: &str) -> String
{
    text.lines().filter(|l| !l.trim_start().starts_with("written =")).collect::<Vec<_>>().join("\n")
}

/// Where older inboxes are kept.
fn backups_dir(fs_: &SafeFs) -> PathBuf
{
    fs_.root(Root::AppData).join("backups")
}

/// Copies Sterling's and the companion's SavedVariables (and the game's .bak copies) for every
/// WTF account into a new backup set. Returns the set's folder, or None if there was nothing to copy.
pub fn backup_saved_variables(fs_: &SafeFs, reason: &str) -> Result<Option<PathBuf>>
{
    let reason: String = reason.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(24).collect();
    let set = backups_dir(fs_).join(format!("{}-{}", now(), if reason.is_empty() { "sync" } else { &reason }));
    let mut copied = 0;

    for acct in paths::wtf_accounts(fs_.game())
    {
        let saved_vars = paths::saved_variables(fs_.game(), &acct);

        for name in ["Sterling.lua", "Sterling.lua.bak", "SterlingQuestSync.lua", "SterlingQuestSync.lua.bak"]
        {
            let src = saved_vars.join(name);

            if src.is_file()
            {
                fs_.copy_in(&src, &set.join(&acct).join(name))?;
                copied += 1;
            }
        }
    }

    if copied == 0
    {
        return Ok(None);
    }

    prune_backups(fs_)?;
    Ok(Some(set))
}

/// Backup sets, newest first.
pub fn list_backups(fs_: &SafeFs) -> Vec<PathBuf>
{
    let mut backups: Vec<(u64, PathBuf)> = fs::read_dir(backups_dir(fs_))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|entry|
        {
            let name = entry.file_name().to_string_lossy().into_owned();

            name.split('-').next()?.parse::<u64>().ok().map(|t| (t, entry.path()))
        })
        .collect();

    backups.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    backups.into_iter().map(|(_, p)| p).collect()
}

/// Keeps only the newest inbox backups.
fn prune_backups(fs_: &SafeFs) -> Result<()>
{
    for old in list_backups(fs_).into_iter().skip(KEEP_BACKUPS)
    {
        fs_.remove_scratch(&old)?;
    }

    Ok(())
}

/// "Delete" inside the allowlist: moves into the app's trash folder instead.
pub fn move_to_trash(fs_: &SafeFs, path: &Path) -> Result<PathBuf>
{
    let name = path.file_name().ok_or_else(|| Error::Refused("nothing to trash".into()))?;
    let dst = fs_.root(Root::AppData).join("trash").join(format!("{}-{}", now(), name.to_string_lossy()));

    fs_.rename(path, &dst)?;
    Ok(dst)
}

#[cfg(test)]
mod tests
{
    use super::*;
    use crate::formats::{InPacket, Kind};

    fn setup() -> (tempfile::TempDir, SafeFs)
    {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("_forever_");

        fs::create_dir_all(game.join("Interface/AddOns/Questie")).unwrap();
        fs::write(game.join("Interface/AddOns/Questie/Questie.toc"), "x").unwrap();

        let saved_vars = game.join("WTF/Account/TESTACCOUNT/SavedVariables");

        fs::create_dir_all(&saved_vars).unwrap();
        fs::write(saved_vars.join("Sterling.lua"), "SterlingDB = {}").unwrap();
        fs::write(saved_vars.join("Sterling.lua.bak"), "SterlingDB = {}").unwrap();
        fs::write(saved_vars.join("Questie.lua"), "QuestieDB = {}").unwrap();

        let safe = SafeFs::new(&game, &temp.path().join("appdata")).unwrap();
        (temp, safe)
    }

    #[test]
    fn installs_the_companion_and_an_empty_inbox()
    {
        let (_t, safe) = setup();

        install(&safe, &[("SterlingQuestSync.toc", crate::formats::companion_toc().as_bytes()), ("Core.lua", b"-- core")]).unwrap();
        assert!(is_installed(&safe));
        assert!(install(&safe, &[("../Questie/Questie.toc", b"x")]).is_err());
        assert!(install(&safe, &[("Inbox.lua", b"x")]).is_err());
        assert!(install(&safe, &[("evil.exe", b"x")]).is_err());
        assert_eq!(fs::read_to_string(safe.game().join("Interface/AddOns/Questie/Questie.toc")).unwrap(), "x");
    }

    #[test]
    fn inbox_is_only_rewritten_when_it_changes()
    {
        let (_t, safe) = setup();
        let mut inbox = Inbox::default();

        inbox.put_packet
        (
            "Player-6800-0000A11C",
            InPacket { seq: 1, from: "Player-6800-0A1B2C3D".into(), kind: Kind::Partner, time: 1794770000, proto: 6, data: "AB".into() },
        );

        assert!(write_inbox(&safe, &inbox, 1794770000).unwrap());
        assert!(!write_inbox(&safe, &inbox, 1794770999).unwrap());
        inbox.put_packet
        (
            "Player-6800-0000A11C",
            InPacket { seq: 2, from: "Player-6800-0A1B2C3D".into(), kind: Kind::Partner, time: 1794770100, proto: 6, data: "CD".into() },
        );

        assert!(write_inbox(&safe, &inbox, 1794771000).unwrap());
    }

    #[test]
    fn backups_copy_from_wtf_and_leave_it_untouched()
    {
        let (_t, safe) = setup();
        let saved_vars = safe.game().join("WTF/Account/TESTACCOUNT/SavedVariables");
        let before: Vec<_> = fs::read_dir(&saved_vars).unwrap().flatten().map(|e| e.file_name()).collect();
        let set = backup_saved_variables(&safe, "test").unwrap().unwrap();

        assert!(set.join("TESTACCOUNT/Sterling.lua").is_file());
        assert!(set.join("TESTACCOUNT/Sterling.lua.bak").is_file());
        assert!(!set.join("TESTACCOUNT/Questie.lua").exists(), "only Sterling's files");

        let after: Vec<_> = fs::read_dir(&saved_vars).unwrap().flatten().map(|e| e.file_name()).collect();

        assert_eq!(before.len(), after.len());
        assert_eq!(fs::read_to_string(saved_vars.join("Sterling.lua")).unwrap(), "SterlingDB = {}");
    }

    #[test]
    fn old_backups_are_pruned()
    {
        let (_t, safe) = setup();

        for i in 0..(KEEP_BACKUPS + 5)
        {
            fs::create_dir_all(backups_dir(&safe).join(format!("{}-old", 1000 + i))).unwrap();
        }

        backup_saved_variables(&safe, "x").unwrap();
        assert_eq!(list_backups(&safe).len(), KEEP_BACKUPS);
    }

    #[test]
    fn trash_refuses_wtf_and_other_addons()
    {
        let (_t, safe) = setup();

        assert!(move_to_trash(&safe, &safe.game().join("WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua")).is_err());
        assert!(move_to_trash(&safe, &safe.game().join("Interface/AddOns/Questie")).is_err());
        install(&safe, &[("SterlingQuestSync.toc", b"x")]).unwrap();

        let moved = move_to_trash(&safe, &safe.root(Root::Companion).join("SterlingQuestSync.toc")).unwrap();

        assert!(moved.is_file());
    }

    #[test]
    fn notices_when_the_game_saved_without_the_helper()
    {
        use std::time::{Duration, SystemTime};

        let temp = tempfile::tempdir().unwrap();
        let game = temp.path();
        let saved_vars = game.join("WTF/Account/TESTACCOUNT/SavedVariables");

        fs::create_dir_all(&saved_vars).unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/SterlingQuestSync")).unwrap();

        let toc = game.join("Interface/AddOns/SterlingQuestSync/SterlingQuestSync.toc");

        fs::write(&toc, "x").unwrap();

        let set = |path: &Path, secs: u64|
        {
            let opened = fs::File::options().write(true).open(path).unwrap();

            opened.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
        };

        set(&toc, 1_000_000);
        // No save since the helper was added: not a problem yet.
        assert!(!helper_not_loaded(game));

        let sterling = saved_vars.join("Sterling.lua");

        fs::write(&sterling, "x").unwrap();
        set(&sterling, 1_000_500);
        // Sterling saved, the helper didn't: not loaded.
        assert!(helper_not_loaded(game));

        let helper = saved_vars.join("SterlingQuestSync.lua");

        fs::write(&helper, "x").unwrap();
        set(&helper, 1_000_100);
        // The helper's file is from an older session: still not loaded now.
        assert!(helper_not_loaded(game));
        set(&helper, 1_000_499);
        // Saved together: loaded.
        assert!(!helper_not_loaded(game));
        // An older Sterling save from before the helper was added doesn't count.
        set(&sterling, 999_000);
        assert!(!helper_not_loaded(game));
    }
}
