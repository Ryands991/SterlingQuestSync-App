//! Every file change the app makes goes through `SafeFs`.
//!
//! Hard rules (decided Nov 15, not settings):
//! - Writes only inside: `AddOns/SterlingQuestSync`, `AddOns/Sterling`, and the app's own data folder.
//! - `WTF` is read-only, no exceptions. Checked first, after resolving `..`, links and junctions.
//! - Nothing is deleted outside the app's own folders. Replacing Sterling moves the old copy aside.
//! - The only other write is a file the person picks in a Save dialog (recovery code), and even that
//!   refuses anywhere inside the game folder.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root
{
    Companion,
    Sterling,
    AppData,
}

#[derive(Debug, Clone)]
pub struct SafeFs
{
    game: PathBuf,
    wtf: PathBuf,
    companion: PathBuf,
    sterling: PathBuf,
    app_data: PathBuf,
}

/// Rejects relative paths and any `..` or `.` component outright. No lexical tricks get through.
fn plain_absolute(p: &Path) -> Result<()>
{
    if !p.is_absolute()
    {
        return Err(Error::Refused(format!("not an absolute path: {}", p.display())));
    }

    for c in p.components()
    {
        match c
        {
            Component::ParentDir | Component::CurDir => return Err(Error::Refused("path contains . or ..".into())),
            // A colon past the drive letter is a Windows alternate stream ("WTF::$INDEX_ALLOCATION").
            Component::Normal(n) if n.to_string_lossy().contains(':') => return Err(Error::Refused("path contains a colon".into())),
            _ => {}
        }
    }

    Ok(())
}

/// Resolves a path that may not exist yet: canonicalizes the deepest existing ancestor,
/// refusing if any existing component is a symlink or junction.
fn resolve(p: &Path) -> Result<PathBuf>
{
    plain_absolute(p)?;

    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();

    loop
    {
        match fs::symlink_metadata(&existing)
        {
            Ok(_) => break,
            Err(_) =>
            {
                let name = existing.file_name().ok_or_else(|| Error::Refused("path has no existing ancestor".into()))?.to_os_string();

                rest.push(name);

                if !existing.pop()
                {
                    return Err(Error::Refused("path has no existing ancestor".into()));
                }
            }
        }
    }

    // Walk every existing component and refuse links anywhere on the way.
    let mut walk = PathBuf::new();

    for c in existing.components()
    {
        walk.push(c);

        if let Ok(md) = fs::symlink_metadata(&walk)
        {
            if md.file_type().is_symlink() || is_reparse_point(&md)
            {
                return Err(Error::Refused(format!("link or junction in path: {}", walk.display())));
            }
        }
    }

    let mut out = dunce(fs::canonicalize(&existing)?);

    for name in rest.into_iter().rev()
    {
        out.push(name);
    }

    Ok(out)
}

#[cfg(windows)]
fn is_reparse_point(md: &fs::Metadata) -> bool
{
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_md: &fs::Metadata) -> bool
{
    false
}

/// Strips Windows' `\\?\` prefix so comparisons work on plain paths.
fn dunce(p: PathBuf) -> PathBuf
{
    let s = p.to_string_lossy();

    if let Some(rest) = s.strip_prefix(r"\\?\")
    {
        if !rest.starts_with("UNC\\")
        {
            return PathBuf::from(rest);
        }
    }

    p
}

/// Case-insensitive on Windows and macOS (default file systems), exact on Linux.
fn key(p: &Path) -> String
{
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();

    if cfg!(any(windows, target_os = "macos")) { s.to_lowercase() } else { s }
}

fn is_within(child: &Path, parent: &Path) -> bool
{
    let c = key(child);
    let p = key(parent);

    c == p || c.starts_with(&(p + "/"))
}

/// Settings and other files the app keeps before a game folder is chosen.
/// Only a plain file name directly inside the app's data folder.
pub fn write_app_file(app_data: &Path, name: &str, bytes: &[u8]) -> Result<()>
{
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && !name.starts_with('.');

    if !ok
    {
        return Err(Error::Refused("bad app file name".into()));
    }

    fs::create_dir_all(app_data)?;

    let dir = resolve(app_data)?;
    let target = dir.join(name);

    if target.is_symlink()
    {
        return Err(Error::Refused("refusing a link".into()));
    }

    let tmp = dir.join(format!(".{}.sqs-tmp-{}", name, rand::random::<u32>()));
    {
        let mut f = fs::File::create(&tmp)?;

        f.write_all(bytes)?;
        f.sync_all()?;
    }

    if let Err(e) = fs::rename(&tmp, &target)
    {
        let _ = fs::remove_file(&tmp);

        return Err(e.into());
    }

    Ok(())
}

impl SafeFs
{
    /// `game` is the flavour folder, for example `.../World of Warcraft/_forever_`.
    pub fn new(game: &Path, app_data: &Path) -> Result<Self>
    {
        fs::create_dir_all(app_data)?;

        let game = resolve(game)?;
        let addons = game.join("Interface").join("AddOns");

        Ok(SafeFs { wtf: game.join("WTF"), companion: addons.join("SterlingQuestSync"), sterling: addons.join("Sterling"), app_data: resolve(app_data)?, game })
    }

    pub fn root(&self, r: Root) -> &Path
    {
        match r
        {
            Root::Companion => &self.companion,
            Root::Sterling => &self.sterling,
            Root::AppData => &self.app_data,
        }
    }

    pub fn game(&self) -> &Path
    {
        &self.game
    }

    pub fn wtf(&self) -> &Path
    {
        &self.wtf
    }

    /// The single gate. Returns the resolved path if a write there is allowed.
    pub fn check_write(&self, p: &Path) -> Result<PathBuf>
    {
        let r = resolve(p)?;

        // WTF first, before anything else can say yes.
        if is_within(&r, &self.wtf)
        {
            return Err(Error::Refused("WTF is read-only for this app".into()));
        }

        for root in [&self.companion, &self.sterling, &self.app_data]
        {
            if is_within(&r, root)
            {
                return Ok(r);
            }
        }

        Err(Error::Refused(format!("outside the app's write allowlist: {}", r.display())))
    }

    /// For a file the person picked in a Save dialog: anywhere except inside the game folder.
    pub fn check_user_export(&self, p: &Path) -> Result<PathBuf>
    {
        let r = resolve(p)?;

        if is_within(&r, &self.game) || is_within(&r, &self.wtf)
        {
            return Err(Error::Refused("can't save inside the game folder".into()));
        }

        Ok(r)
    }

    pub fn create_dir_all(&self, p: &Path) -> Result<()>
    {
        let r = self.check_write(p)?;

        fs::create_dir_all(r)?;
        Ok(())
    }

    /// Writes a file so a reader never sees half of it: temp file in the same folder, flush, then rename.
    pub fn write_atomic(&self, p: &Path, bytes: &[u8]) -> Result<()>
    {
        let target = self.check_write(p)?;
        let dir = target.parent().ok_or_else(|| Error::Refused("no parent folder".into()))?;

        self.create_dir_all(dir)?;

        let name = target.file_name().unwrap().to_string_lossy().to_string();
        let tmp = dir.join(format!(".{}.sqs-tmp-{}", name, rand::random::<u32>()));
        let tmp = self.check_write(&tmp)?;
        {
            let mut f = fs::File::create(&tmp)?;

            f.write_all(bytes)?;
            f.sync_all()?;
        }

        if let Err(e) = fs::rename(&tmp, &target)
        {
            let _ = fs::remove_file(&tmp);

            return Err(e.into());
        }

        Ok(())
    }

    /// Writes a file the person chose in a Save dialog.
    pub fn write_user_export(&self, p: &Path, bytes: &[u8]) -> Result<()>
    {
        let target = self.check_user_export(p)?;

        fs::write(target, bytes)?;
        Ok(())
    }

    /// Renames within the allowlist. Both ends are checked.
    pub fn rename(&self, from: &Path, to: &Path) -> Result<()>
    {
        let a = self.check_write(from)?;
        let b = self.check_write(to)?;

        if let Some(parent) = b.parent()
        {
            if !parent.is_dir()
            {
                self.create_dir_all(parent)?;
            }
        }

        fs::rename(a, b)?;
        Ok(())
    }

    /// Copies a file from anywhere readable (including WTF) to an allowed place.
    pub fn copy_in(&self, src: &Path, dst: &Path) -> Result<()>
    {
        let d = self.check_write(dst)?;

        if let Some(parent) = d.parent()
        {
            self.create_dir_all(parent)?;
        }

        let s = resolve(src)?;

        fs::copy(s, d)?;
        Ok(())
    }

    /// Removes a folder tree, but only the app's own scratch areas:
    /// anything in the app data folder, or `_staging` / `_old` inside the companion add-on.
    pub fn remove_scratch(&self, p: &Path) -> Result<()>
    {
        let r = self.check_write(p)?;
        let ok = (is_within(&r, &self.app_data) && key(&r) != key(&self.app_data))
            || is_within(&r, &self.companion.join("_staging"))
            || is_within(&r, &self.companion.join("_old"));

        if !ok
        {
            return Err(Error::Refused("only the app's own scratch folders can be removed".into()));
        }

        if r.is_dir()
        {
            fs::remove_dir_all(r)?;
        }

        else if r.exists()
        {
            fs::remove_file(r)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn setup() -> (tempfile::TempDir, SafeFs)
    {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("World of Warcraft").join("_forever_");

        fs::create_dir_all(game.join("Interface/AddOns/Sterling")).unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/Questie")).unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/SterlingQuestSync")).unwrap();
        fs::create_dir_all(game.join("WTF/Account/TESTACCOUNT/SavedVariables")).unwrap();
        fs::write(game.join("WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua"), "SterlingQuestDB = {}").unwrap();
        fs::write(game.join("Interface/AddOns/Questie/Questie.toc"), "## Title: Questie").unwrap();

        let app = t.path().join("AppData").join("SterlingQuestSync");
        let s = SafeFs::new(&game, &app).unwrap();
        (t, s)
    }

    #[test]
    fn allows_the_three_roots()
    {
        let (_t, s) = setup();

        s.write_atomic(&s.root(Root::Companion).join("Inbox.lua"), b"x").unwrap();
        s.write_atomic(&s.root(Root::Sterling).join("Core.lua"), b"x").unwrap();
        s.write_atomic(&s.root(Root::AppData).join("state.json"), b"{}").unwrap();
    }

    #[test]
    fn wtf_is_read_only_through_every_function()
    {
        let (_t, s) = setup();
        let sv = s.wtf().join("Account/TESTACCOUNT/SavedVariables/Sterling.lua");
        let new = s.wtf().join("Account/TESTACCOUNT/SavedVariables/New.lua");

        assert!(s.check_write(&sv).is_err());
        assert!(s.write_atomic(&sv, b"bad").is_err());
        assert!(s.write_atomic(&new, b"bad").is_err());
        assert!(s.create_dir_all(&s.wtf().join("Account/X")).is_err());
        assert!(s.rename(&sv, &s.root(Root::AppData).join("x")).is_err());
        assert!(s.rename(&s.root(Root::AppData).join("state.json"), &new).is_err());
        assert!(s.copy_in(&sv, &new).is_err());
        assert!(s.remove_scratch(&sv).is_err());
        assert!(s.remove_scratch(s.wtf()).is_err());
        assert!(s.write_user_export(&new, b"bad").is_err());
        assert_eq!(fs::read_to_string(&sv).unwrap(), "SterlingQuestDB = {}");
        assert!(!new.exists());
    }

    #[test]
    fn wtf_check_ignores_case_where_the_os_does()
    {
        let (_t, s) = setup();
        let sneaky = PathBuf::from(s.game().to_string_lossy().to_string()).join("wtf").join("Account").join("x.lua");

        if cfg!(any(windows, target_os = "macos"))
        {
            assert!(s.check_write(&sneaky).is_err());
        }

        else
        {
            // Case-sensitive file system: "wtf" is a different folder, and still outside the allowlist.
            assert!(s.check_write(&sneaky).is_err());
        }
    }

    #[test]
    fn refuses_dot_dot_and_relative_paths()
    {
        let (_t, s) = setup();
        let escape = s.root(Root::Companion).join("..").join("..").join("..").join("WTF").join("x.lua");

        assert!(s.check_write(&escape).is_err());

        let escape2 = s.root(Root::AppData).join("..").join("evil.txt");

        assert!(s.check_write(&escape2).is_err());
        assert!(s.check_write(Path::new("relative/Inbox.lua")).is_err());
    }

    #[test]
    fn other_addons_and_the_addons_folder_are_off_limits()
    {
        let (_t, s) = setup();
        let addons = s.game().join("Interface/AddOns");

        assert!(s.write_atomic(&addons.join("Questie/Questie.toc"), b"bad").is_err());
        assert!(s.write_atomic(&addons.join("Evil.lua"), b"bad").is_err());
        assert!(s.create_dir_all(&addons.join("NewAddon")).is_err());
        assert!(s.remove_scratch(&addons.join("Questie")).is_err());
        assert!(s.remove_scratch(&addons.join("Sterling")).is_err());
        assert!(s.remove_scratch(s.root(Root::Companion)).is_err());
        // A folder named like ours but longer is not ours.
        assert!(s.write_atomic(&addons.join("SterlingQuestSyncEvil/x.lua"), b"bad").is_err());
        assert!(s.write_atomic(&addons.join("SterlingHunter/x.lua"), b"bad").is_err());
        assert_eq!(fs::read_to_string(addons.join("Questie/Questie.toc")).unwrap(), "## Title: Questie");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_into_wtf()
    {
        let (_t, s) = setup();
        let link = s.root(Root::Companion).join("sneaky");

        std::os::unix::fs::symlink(s.wtf(), &link).unwrap();
        assert!(s.write_atomic(&link.join("Account/TESTACCOUNT/SavedVariables/Sterling.lua"), b"bad").is_err());

        let link2 = s.root(Root::AppData).join("sneaky2");

        std::os::unix::fs::symlink(s.wtf().join("Account"), &link2).unwrap();
        assert!(s.write_atomic(&link2.join("x.lua"), b"bad").is_err());
        assert_eq!(fs::read_to_string(s.wtf().join("Account/TESTACCOUNT/SavedVariables/Sterling.lua")).unwrap(), "SterlingQuestDB = {}");
    }

    #[test]
    fn scratch_removal_is_limited()
    {
        let (_t, s) = setup();
        let staging = s.root(Root::Companion).join("_staging/0.2.38");

        s.create_dir_all(&staging).unwrap();
        s.remove_scratch(&staging).unwrap();
        assert!(s.remove_scratch(&s.root(Root::Companion).join("Inbox.lua")).is_err());
        assert!(s.remove_scratch(s.root(Root::AppData)).is_err());

        let tmp = s.root(Root::AppData).join("staging");

        s.create_dir_all(&tmp).unwrap();
        s.remove_scratch(&tmp).unwrap();
    }

    #[test]
    fn user_export_refuses_game_folder()
    {
        let (t, s) = setup();

        assert!(s.write_user_export(&s.root(Root::Companion).join("code.txt"), b"x").is_err());
        assert!(s.write_user_export(&s.game().join("code.txt"), b"x").is_err());

        let ok = t.path().join("Documents").join("sterling-quest-sync-recovery.txt");

        fs::create_dir_all(ok.parent().unwrap()).unwrap();
        s.write_user_export(&ok, b"SQS-XXXX").unwrap();
    }

    #[test]
    fn atomic_write_leaves_no_temp_files()
    {
        let (_t, s) = setup();
        let p = s.root(Root::Companion).join("Inbox.lua");

        s.write_atomic(&p, b"one").unwrap();
        s.write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");

        let leftovers: Vec<_> =
            fs::read_dir(s.root(Root::Companion)).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains("sqs-tmp")).collect();

        assert!(leftovers.is_empty());
    }

    #[test]
    fn refuses_alternate_streams()
    {
        let (_t, s) = setup();
        let ads = PathBuf::from(format!("{}::$INDEX_ALLOCATION", s.wtf().display())).join("x.lua");

        assert!(s.check_write(&ads).is_err());

        let ads2 = PathBuf::from(format!("{}:evil", s.root(Root::Companion).join("Inbox.lua").display()));

        assert!(s.check_write(&ads2).is_err());
    }

    // ---- Windows only: real junctions, 8.3 short names, trailing dots. These run on the Windows
    // build machine on every build (GitHub Actions runs `cargo test` there).

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path)
    {
        let out = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().expect("cmd");

        assert!(out.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[cfg(windows)]
    fn short_name(p: &Path) -> PathBuf
    {
        use std::os::windows::process::CommandExt;

        // raw_arg: cmd reads its own quoting, not the C runtime's.
        let out = std::process::Command::new("cmd").raw_arg(format!("/C for %I in (\"{}\") do @echo %~sI", p.display())).output().expect("cmd");

        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    #[cfg(windows)]
    fn sterling_lua(s: &SafeFs) -> String
    {
        fs::read_to_string(s.wtf().join("Account/TESTACCOUNT/SavedVariables/Sterling.lua")).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn windows_junctions_into_wtf_are_refused()
    {
        let (_t, s) = setup();
        // A junction inside our own folder that points into WTF.
        let j = s.root(Root::Companion).join("sneaky");

        junction(&j, s.wtf());
        assert!(s.write_atomic(&j.join("Account/TESTACCOUNT/SavedVariables/Sterling.lua"), b"bad").is_err());
        assert!(s.write_atomic(&j.join("New.lua"), b"bad").is_err());
        assert!(s.create_dir_all(&j.join("Made")).is_err());

        // One in the app's data folder too.
        let j2 = s.root(Root::AppData).join("sneaky2");

        junction(&j2, &s.wtf().join("Account"));
        assert!(s.write_atomic(&j2.join("x.lua"), b"bad").is_err());
        assert!(s.remove_scratch(&j2).is_err(), "never follows a junction to delete");
        assert_eq!(sterling_lua(&s), "SterlingQuestDB = {}");
        assert!(!s.wtf().join("New.lua").exists() && !s.wtf().join("Made").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_companion_folder_swapped_for_a_junction_is_refused()
    {
        let (_t, s) = setup();

        // Someone replaces the whole SterlingQuestSync folder with a junction to SavedVariables.
        fs::remove_dir_all(s.root(Root::Companion)).unwrap();
        junction(s.root(Root::Companion), &s.wtf().join("Account/TESTACCOUNT/SavedVariables"));
        assert!(s.write_atomic(&s.root(Root::Companion).join("Inbox.lua"), b"bad").is_err());
        assert!(s.write_atomic(&s.root(Root::Companion).join("Sterling.lua"), b"bad").is_err());
        assert_eq!(sterling_lua(&s), "SterlingQuestDB = {}");
        assert!(!s.wtf().join("Account/TESTACCOUNT/SavedVariables/Inbox.lua").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_short_names_and_odd_spellings_still_find_wtf()
    {
        let (_t, s) = setup();
        let sv = s.wtf().join("Account/TESTACCOUNT/SavedVariables");
        // 8.3 short form of the game folder ("WORLDO~1\_FOREV~1"), when the drive has short names.
        let short_game = short_name(s.game());

        eprintln!("game folder short name: {}", short_game.display());
        assert!(s.check_write(&short_game.join("WTF").join("x.lua")).is_err());
        assert!(s.write_atomic(&short_game.join("WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua"), b"bad").is_err());

        let short_sv = short_name(&sv);

        assert!(s.write_atomic(&short_sv.join("Sterling.lua"), b"bad").is_err());

        // Letter case, trailing dots and spaces, forward slashes.
        for spelled in ["Wtf", "wTF", "WTF.", "WTF ", "WTF. .", "wtf..."]
        {
            let p = PathBuf::from(format!("{}\\{}\\Account\\TESTACCOUNT\\SavedVariables\\Sterling.lua", s.game().display(), spelled));

            assert!(s.write_atomic(&p, b"bad").is_err(), "{spelled}");
        }

        let fwd = PathBuf::from(format!("{}/WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua", s.game().display()));

        assert!(s.write_atomic(&fwd, b"bad").is_err());

        // The short form of our own folder still works (no false alarms).
        let short_comp = short_name(s.root(Root::Companion));

        s.write_atomic(&short_comp.join("Inbox.lua"), b"ok").unwrap();
        assert_eq!(sterling_lua(&s), "SterlingQuestDB = {}");
    }
}
