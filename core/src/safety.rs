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
fn plain_absolute(checked: &Path) -> Result<()>
{
    if !checked.is_absolute()
    {
        return Err(Error::Refused(format!("not an absolute path: {}", checked.display())));
    }

    for part in checked.components()
    {
        match part
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
fn resolve(wanted: &Path) -> Result<PathBuf>
{
    plain_absolute(wanted)?;

    let mut existing = wanted.to_path_buf();
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

    for part in existing.components()
    {
        walk.push(part);

        if let Ok(meta) = fs::symlink_metadata(&walk)
        {
            if meta.file_type().is_symlink() || is_reparse_point(&meta)
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
fn is_reparse_point(meta: &fs::Metadata) -> bool
{
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_md: &fs::Metadata) -> bool
{
    false
}

/// Strips Windows' `\\?\` prefix so comparisons work on plain paths.
fn dunce(path: PathBuf) -> PathBuf
{
    let text = path.to_string_lossy();

    if let Some(rest) = text.strip_prefix(r"\\?\")
    {
        if !rest.starts_with("UNC\\")
        {
            return PathBuf::from(rest);
        }
    }

    path
}

/// Case-insensitive on Windows and macOS (default file systems), exact on Linux.
fn key(path: &Path) -> String
{
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/').to_string();

    if cfg!(any(windows, target_os = "macos")) { text.to_lowercase() } else { text }
}

/// Is the child path inside the parent?
fn is_within(child: &Path, parent: &Path) -> bool
{
    let child_key = key(child);
    let parent_key = key(parent);

    child_key == parent_key || child_key.starts_with(&(parent_key + "/"))
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
        let mut file = fs::File::create(&tmp)?;

        file.write_all(bytes)?;
        file.sync_all()?;
    }

    if let Err(error) = fs::rename(&tmp, &target)
    {
        let _ = fs::remove_file(&tmp);

        return Err(error.into());
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

    pub fn root(&self, which: Root) -> &Path
    {
        match which
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
    pub fn check_write(&self, path: &Path) -> Result<PathBuf>
    {
        let real = resolve(path)?;

        // WTF first, before anything else can say yes.
        if is_within(&real, &self.wtf)
        {
            return Err(Error::Refused("WTF is read-only for this app".into()));
        }

        for root in [&self.companion, &self.sterling, &self.app_data]
        {
            if is_within(&real, root)
            {
                return Ok(real);
            }
        }

        Err(Error::Refused(format!("outside the app's write allowlist: {}", real.display())))
    }

    /// For a file the person picked in a Save dialog: anywhere except inside the game folder.
    pub fn check_user_export(&self, path: &Path) -> Result<PathBuf>
    {
        let real = resolve(path)?;

        if is_within(&real, &self.game) || is_within(&real, &self.wtf)
        {
            return Err(Error::Refused("can't save inside the game folder".into()));
        }

        Ok(real)
    }

    pub fn create_dir_all(&self, path: &Path) -> Result<()>
    {
        let real = self.check_write(path)?;

        fs::create_dir_all(real)?;
        Ok(())
    }

    /// Writes a file so a reader never sees half of it: temp file in the same folder, flush, then rename.
    pub fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()>
    {
        let target = self.check_write(path)?;
        let dir = target.parent().ok_or_else(|| Error::Refused("no parent folder".into()))?;

        self.create_dir_all(dir)?;

        let name = target.file_name().unwrap().to_string_lossy().to_string();
        let tmp = dir.join(format!(".{}.sqs-tmp-{}", name, rand::random::<u32>()));
        let tmp = self.check_write(&tmp)?;
        {
            let mut file_out = fs::File::create(&tmp)?;

            file_out.write_all(bytes)?;
            file_out.sync_all()?;
        }

        if let Err(error) = fs::rename(&tmp, &target)
        {
            let _ = fs::remove_file(&tmp);

            return Err(error.into());
        }

        Ok(())
    }

    /// Writes a file the person chose in a Save dialog.
    pub fn write_user_export(&self, path: &Path, bytes: &[u8]) -> Result<()>
    {
        let target = self.check_user_export(path)?;

        fs::write(target, bytes)?;
        Ok(())
    }

    /// Renames within the allowlist. Both ends are checked.
    pub fn rename(&self, from: &Path, to: &Path) -> Result<()>
    {
        let source = self.check_write(from)?;
        let dest = self.check_write(to)?;

        if let Some(parent) = dest.parent()
        {
            if !parent.is_dir()
            {
                self.create_dir_all(parent)?;
            }
        }

        fs::rename(source, dest)?;
        Ok(())
    }

    /// Copies a file from anywhere readable (including WTF) to an allowed place.
    pub fn copy_in(&self, src: &Path, dst: &Path) -> Result<()>
    {
        let dest = self.check_write(dst)?;

        if let Some(parent) = dest.parent()
        {
            self.create_dir_all(parent)?;
        }

        let source = resolve(src)?;

        fs::copy(source, dest)?;
        Ok(())
    }

    /// Removes a folder tree, but only the app's own scratch areas:
    /// anything in the app data folder, or `_staging` / `_old` inside the companion add-on.
    pub fn remove_scratch(&self, path: &Path) -> Result<()>
    {
        let real = self.check_write(path)?;
        let ok = (is_within(&real, &self.app_data) && key(&real) != key(&self.app_data))
            || is_within(&real, &self.companion.join("_staging"))
            || is_within(&real, &self.companion.join("_old"));

        if !ok
        {
            return Err(Error::Refused("only the app's own scratch folders can be removed".into()));
        }

        if real.is_dir()
        {
            fs::remove_dir_all(real)?;
        }

        else if real.exists()
        {
            fs::remove_file(real)?;
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
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("World of Warcraft").join("_forever_");

        fs::create_dir_all(game.join("Interface/AddOns/Sterling")).unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/Questie")).unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/SterlingQuestSync")).unwrap();
        fs::create_dir_all(game.join("WTF/Account/TESTACCOUNT/SavedVariables")).unwrap();
        fs::write(game.join("WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua"), "SterlingQuestDB = {}").unwrap();
        fs::write(game.join("Interface/AddOns/Questie/Questie.toc"), "## Title: Questie").unwrap();

        let app = temp.path().join("AppData").join("SterlingQuestSync");
        let safe = SafeFs::new(&game, &app).unwrap();
        (temp, safe)
    }

    #[test]
    fn allows_the_three_roots()
    {
        let (_t, safe) = setup();

        safe.write_atomic(&safe.root(Root::Companion).join("Inbox.lua"), b"x").unwrap();
        safe.write_atomic(&safe.root(Root::Sterling).join("Core.lua"), b"x").unwrap();
        safe.write_atomic(&safe.root(Root::AppData).join("state.json"), b"{}").unwrap();
    }

    #[test]
    fn wtf_is_read_only_through_every_function()
    {
        let (_t, safe) = setup();
        let saved_vars = safe.wtf().join("Account/TESTACCOUNT/SavedVariables/Sterling.lua");
        let new = safe.wtf().join("Account/TESTACCOUNT/SavedVariables/New.lua");

        assert!(safe.check_write(&saved_vars).is_err());
        assert!(safe.write_atomic(&saved_vars, b"bad").is_err());
        assert!(safe.write_atomic(&new, b"bad").is_err());
        assert!(safe.create_dir_all(&safe.wtf().join("Account/X")).is_err());
        assert!(safe.rename(&saved_vars, &safe.root(Root::AppData).join("x")).is_err());
        assert!(safe.rename(&safe.root(Root::AppData).join("state.json"), &new).is_err());
        assert!(safe.copy_in(&saved_vars, &new).is_err());
        assert!(safe.remove_scratch(&saved_vars).is_err());
        assert!(safe.remove_scratch(safe.wtf()).is_err());
        assert!(safe.write_user_export(&new, b"bad").is_err());
        assert_eq!(fs::read_to_string(&saved_vars).unwrap(), "SterlingQuestDB = {}");
        assert!(!new.exists());
    }

    #[test]
    fn wtf_check_ignores_case_where_the_os_does()
    {
        let (_t, safe) = setup();
        let sneaky = PathBuf::from(safe.game().to_string_lossy().to_string()).join("wtf").join("Account").join("x.lua");

        if cfg!(any(windows, target_os = "macos"))
        {
            assert!(safe.check_write(&sneaky).is_err());
        }

        else
        {
            // Case-sensitive file system: "wtf" is a different folder, and still outside the allowlist.
            assert!(safe.check_write(&sneaky).is_err());
        }
    }

    #[test]
    fn refuses_dot_dot_and_relative_paths()
    {
        let (_t, safe) = setup();
        let escape = safe.root(Root::Companion).join("..").join("..").join("..").join("WTF").join("x.lua");

        assert!(safe.check_write(&escape).is_err());

        let escape2 = safe.root(Root::AppData).join("..").join("evil.txt");

        assert!(safe.check_write(&escape2).is_err());
        assert!(safe.check_write(Path::new("relative/Inbox.lua")).is_err());
    }

    #[test]
    fn other_addons_and_the_addons_folder_are_off_limits()
    {
        let (_t, safe) = setup();
        let addons = safe.game().join("Interface/AddOns");

        assert!(safe.write_atomic(&addons.join("Questie/Questie.toc"), b"bad").is_err());
        assert!(safe.write_atomic(&addons.join("Evil.lua"), b"bad").is_err());
        assert!(safe.create_dir_all(&addons.join("NewAddon")).is_err());
        assert!(safe.remove_scratch(&addons.join("Questie")).is_err());
        assert!(safe.remove_scratch(&addons.join("Sterling")).is_err());
        assert!(safe.remove_scratch(safe.root(Root::Companion)).is_err());
        // A folder named like ours but longer is not ours.
        assert!(safe.write_atomic(&addons.join("SterlingQuestSyncEvil/x.lua"), b"bad").is_err());
        assert!(safe.write_atomic(&addons.join("SterlingHunter/x.lua"), b"bad").is_err());
        assert_eq!(fs::read_to_string(addons.join("Questie/Questie.toc")).unwrap(), "## Title: Questie");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_into_wtf()
    {
        let (_t, safe) = setup();
        let link = safe.root(Root::Companion).join("sneaky");

        std::os::unix::fs::symlink(safe.wtf(), &link).unwrap();
        assert!(safe.write_atomic(&link.join("Account/TESTACCOUNT/SavedVariables/Sterling.lua"), b"bad").is_err());

        let link2 = safe.root(Root::AppData).join("sneaky2");

        std::os::unix::fs::symlink(safe.wtf().join("Account"), &link2).unwrap();
        assert!(safe.write_atomic(&link2.join("x.lua"), b"bad").is_err());
        assert_eq!(fs::read_to_string(safe.wtf().join("Account/TESTACCOUNT/SavedVariables/Sterling.lua")).unwrap(), "SterlingQuestDB = {}");
    }

    #[test]
    fn scratch_removal_is_limited()
    {
        let (_t, safe) = setup();
        let staging = safe.root(Root::Companion).join("_staging/0.2.38");

        safe.create_dir_all(&staging).unwrap();
        safe.remove_scratch(&staging).unwrap();
        assert!(safe.remove_scratch(&safe.root(Root::Companion).join("Inbox.lua")).is_err());
        assert!(safe.remove_scratch(safe.root(Root::AppData)).is_err());

        let tmp = safe.root(Root::AppData).join("staging");

        safe.create_dir_all(&tmp).unwrap();
        safe.remove_scratch(&tmp).unwrap();
    }

    #[test]
    fn user_export_refuses_game_folder()
    {
        let (temp, safe) = setup();

        assert!(safe.write_user_export(&safe.root(Root::Companion).join("code.txt"), b"x").is_err());
        assert!(safe.write_user_export(&safe.game().join("code.txt"), b"x").is_err());

        let ok = temp.path().join("Documents").join("sterling-quest-sync-recovery.txt");

        fs::create_dir_all(ok.parent().unwrap()).unwrap();
        safe.write_user_export(&ok, b"SQS-XXXX").unwrap();
    }

    #[test]
    fn atomic_write_leaves_no_temp_files()
    {
        let (_t, safe) = setup();
        let path = safe.root(Root::Companion).join("Inbox.lua");

        safe.write_atomic(&path, b"one").unwrap();
        safe.write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");

        let leftovers: Vec<_> =
            fs::read_dir(safe.root(Root::Companion)).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains("sqs-tmp")).collect();

        assert!(leftovers.is_empty());
    }

    #[test]
    fn refuses_alternate_streams()
    {
        let (_t, safe) = setup();
        let ads = PathBuf::from(format!("{}::$INDEX_ALLOCATION", safe.wtf().display())).join("x.lua");

        assert!(safe.check_write(&ads).is_err());

        let ads2 = PathBuf::from(format!("{}:evil", safe.root(Root::Companion).join("Inbox.lua").display()));

        assert!(safe.check_write(&ads2).is_err());
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
    fn short_name(path: &Path) -> PathBuf
    {
        use std::os::windows::process::CommandExt;

        // raw_arg: cmd reads its own quoting, not the C runtime's.
        let out = std::process::Command::new("cmd").raw_arg(format!("/C for %I in (\"{}\") do @echo %~sI", path.display())).output().expect("cmd");

        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    #[cfg(windows)]
    fn sterling_lua(safe: &SafeFs) -> String
    {
        fs::read_to_string(safe.wtf().join("Account/TESTACCOUNT/SavedVariables/Sterling.lua")).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn windows_junctions_into_wtf_are_refused()
    {
        let (_t, safe) = setup();
        // A junction inside our own folder that points into WTF.
        let outside = safe.root(Root::Companion).join("sneaky");

        junction(&outside, safe.wtf());
        assert!(safe.write_atomic(&outside.join("Account/TESTACCOUNT/SavedVariables/Sterling.lua"), b"bad").is_err());
        assert!(safe.write_atomic(&outside.join("New.lua"), b"bad").is_err());
        assert!(safe.create_dir_all(&outside.join("Made")).is_err());

        // One in the app's data folder too.
        let outside2 = safe.root(Root::AppData).join("sneaky2");

        junction(&outside2, &safe.wtf().join("Account"));
        assert!(safe.write_atomic(&outside2.join("x.lua"), b"bad").is_err());
        assert!(safe.remove_scratch(&outside2).is_err(), "never follows a junction to delete");
        assert_eq!(sterling_lua(&safe), "SterlingQuestDB = {}");
        assert!(!safe.wtf().join("New.lua").exists() && !safe.wtf().join("Made").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_companion_folder_swapped_for_a_junction_is_refused()
    {
        let (_t, safe) = setup();

        // Someone replaces the whole SterlingQuestSync folder with a junction to SavedVariables.
        fs::remove_dir_all(safe.root(Root::Companion)).unwrap();
        junction(safe.root(Root::Companion), &safe.wtf().join("Account/TESTACCOUNT/SavedVariables"));
        assert!(safe.write_atomic(&safe.root(Root::Companion).join("Inbox.lua"), b"bad").is_err());
        assert!(safe.write_atomic(&safe.root(Root::Companion).join("Sterling.lua"), b"bad").is_err());
        assert_eq!(sterling_lua(&safe), "SterlingQuestDB = {}");
        assert!(!safe.wtf().join("Account/TESTACCOUNT/SavedVariables/Inbox.lua").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_short_names_and_odd_spellings_still_find_wtf()
    {
        let (_t, safe) = setup();
        let saved_vars = safe.wtf().join("Account/TESTACCOUNT/SavedVariables");
        // 8.3 short form of the game folder ("WORLDO~1\_FOREV~1"), when the drive has short names.
        let short_game = short_name(safe.game());

        eprintln!("game folder short name: {}", short_game.display());
        assert!(safe.check_write(&short_game.join("WTF").join("x.lua")).is_err());
        assert!(safe.write_atomic(&short_game.join("WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua"), b"bad").is_err());

        let short_sv = short_name(&saved_vars);

        assert!(safe.write_atomic(&short_sv.join("Sterling.lua"), b"bad").is_err());

        // Letter case, trailing dots and spaces, forward slashes.
        for spelled in ["Wtf", "wTF", "WTF.", "WTF ", "WTF. .", "wtf..."]
        {
            let path = PathBuf::from(format!("{}\\{}\\Account\\TESTACCOUNT\\SavedVariables\\Sterling.lua", safe.game().display(), spelled));

            assert!(safe.write_atomic(&path, b"bad").is_err(), "{spelled}");
        }

        let fwd = PathBuf::from(format!("{}/WTF/Account/TESTACCOUNT/SavedVariables/Sterling.lua", safe.game().display()));

        assert!(safe.write_atomic(&fwd, b"bad").is_err());

        // The short form of our own folder still works (no false alarms).
        let short_comp = short_name(safe.root(Root::Companion));

        safe.write_atomic(&short_comp.join("Inbox.lua"), b"ok").unwrap();
        assert_eq!(sterling_lua(&safe), "SterlingQuestDB = {}");
    }
}
