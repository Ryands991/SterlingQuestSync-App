//! Finding the game. Read-only: nothing here writes anywhere.
//!
//! A "game folder" is the flavour folder that holds `Interface` and `WTF`, for example
//! `C:\Program Files (x86)\World of Warcraft\_forever_`. WoW Forever installs there by default.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Flavour folder names worth checking under a World of Warcraft folder, most likely first.
const FLAVOURS: &[&str] = &["_forever_", "_classic_era_", "_classic_", "_anniversary_", "_retail_"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GameFolder
{
    pub path: PathBuf,
    /// The flavour folder name, like `_forever_`, or the folder's own name if it isn't one.
    pub flavour: String,
    pub has_sterling: bool,
    pub has_companion: bool,
    /// Installed Sterling version, from its TOC.
    pub sterling_version: Option<String>,
    /// WTF account folder names. Shown only in this PC's UI, never uploaded or logged.
    pub accounts: Vec<String>,
}

/// True when the folder looks like a game flavour folder.
pub fn looks_like_game(p: &Path) -> bool
{
    p.join("Interface").join("AddOns").is_dir() || (p.join("WTF").is_dir() && p.join("Interface").is_dir())
}

pub fn inspect(p: &Path) -> Option<GameFolder>
{
    if !looks_like_game(p)
    {
        return None;
    }

    let addons = p.join("Interface").join("AddOns");

    Some(GameFolder
    {
        path: p.to_path_buf(),
        flavour: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        has_sterling: addons.join("Sterling").join("Sterling.toc").is_file() || addons.join("Sterling").is_dir(),
        has_companion: addons.join("SterlingQuestSync").is_dir(),
        sterling_version: crate::updater::installed_version(p),
        accounts: wtf_accounts(p),
    })
}

/// Accepts the folder the person picked, or a parent of it, and returns the flavour folder.
/// Picking `World of Warcraft` finds `_forever_` inside; picking `Interface` or `AddOns` walks up.
pub fn normalise_pick(p: &Path) -> Option<PathBuf>
{
    if looks_like_game(p)
    {
        return Some(p.to_path_buf());
    }

    for f in FLAVOURS
    {
        let c = p.join(f);

        if looks_like_game(&c)
        {
            return Some(c);
        }
    }

    let mut cur = p;

    for _ in 0..3
    {
        cur = cur.parent()?;

        if looks_like_game(cur)
        {
            return Some(cur.to_path_buf());
        }
    }

    None
}

pub fn wtf_accounts(game: &Path) -> Vec<String>
{
    let mut out = Vec::new();

    if let Ok(rd) = fs::read_dir(game.join("WTF").join("Account"))
    {
        for e in rd.flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();

            if e.path().join("SavedVariables").is_dir() && name != "SavedVariables"
            {
                out.push(name);
            }
        }
    }

    out.sort();
    out
}

/// Account-wide SavedVariables folder. Read only.
pub fn saved_variables(game: &Path, account: &str) -> PathBuf
{
    game.join("WTF").join("Account").join(account).join("SavedVariables")
}

/// The companion's outbox file, written by the game.
pub fn outbox_file(game: &Path, account: &str) -> PathBuf
{
    saved_variables(game, account).join("SterlingQuestSync.lua")
}

/// Sterling's own SavedVariables, copied into backups before every inbox change.
pub fn sterling_saved(game: &Path, account: &str) -> PathBuf
{
    saved_variables(game, account).join("Sterling.lua")
}

pub fn companion_dir(game: &Path) -> PathBuf
{
    game.join("Interface").join("AddOns").join("SterlingQuestSync")
}

pub fn inbox_file(game: &Path) -> PathBuf
{
    companion_dir(game).join("Inbox.lua")
}

/// Common places to look, then any World of Warcraft folder named in the Windows registry.
pub fn candidates() -> Vec<PathBuf>
{
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        for drive in b'C'..=b'H'
        {
            let d = format!("{}:\\", drive as char);

            for sub in
            [
                r"Program Files (x86)\World of Warcraft",
                r"Program Files\World of Warcraft",
                r"World of Warcraft",
                r"Games\World of Warcraft",
                r"Battle.net\World of Warcraft",
            ]

            {
                roots.push(PathBuf::from(&d).join(sub));
            }
        }

        roots.extend(registry_installs());
    }

    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/Applications/World of Warcraft"));

        if let Some(h) = dirs::home_dir()
        {
            roots.push(h.join("Applications/World of Warcraft"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(h) = dirs::home_dir()
        {
            for sub in
            [
                "Games/world-of-warcraft/drive_c/Program Files (x86)/World of Warcraft",
                ".wine/drive_c/Program Files (x86)/World of Warcraft",
                "Games/World of Warcraft",
            ]

            {
                roots.push(h.join(sub));
            }
        }
    }

    roots
}

#[cfg(windows)]
fn registry_installs() -> Vec<PathBuf>
{
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

    let mut out = Vec::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    for k in [r"SOFTWARE\WOW6432Node\Blizzard Entertainment\World of Warcraft", r"SOFTWARE\Blizzard Entertainment\World of Warcraft"]
    {
        if let Ok(key) = hklm.open_subkey_with_flags(k, KEY_READ)
        {
            if let Ok(p) = key.get_value::<String, _>("InstallPath")
            {
                let p = PathBuf::from(p.trim_end_matches(['\\', '/']));

                // InstallPath often points at a flavour folder; check its parent too.
                if let Some(parent) = p.parent()
                {
                    out.push(parent.to_path_buf());
                }

                out.push(p);
            }
        }
    }

    out
}

/// Every game folder found, without duplicates.
pub fn discover() -> Vec<GameFolder>
{
    discover_in(&candidates())
}

pub fn discover_in(roots: &[PathBuf]) -> Vec<GameFolder>
{
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let mut push = |p: PathBuf, out: &mut Vec<GameFolder>|
    {
        let k = fs::canonicalize(&p).unwrap_or(p.clone()).to_string_lossy().to_lowercase();

        if seen.contains(&k)
        {
            return;
        }

        if let Some(g) = inspect(&p)
        {
            seen.push(k);
            out.push(g);
        }
    };

    for r in roots
    {
        if looks_like_game(r)
        {
            push(r.clone(), &mut out);
            continue;
        }

        for f in FLAVOURS
        {
            push(r.join(f), &mut out);
        }
    }

    // _forever_ first, then folders that already have Sterling.
    out.sort_by_key(|g| (g.flavour != "_forever_", !g.has_sterling));
    out
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn fake_install(root: &Path, flavour: &str, sterling: bool) -> PathBuf
    {
        let g = root.join("World of Warcraft").join(flavour);

        fs::create_dir_all(g.join("Interface/AddOns/Questie")).unwrap();
        fs::create_dir_all(g.join("WTF/Account/TESTACCOUNT/SavedVariables")).unwrap();
        fs::create_dir_all(g.join("WTF/Account/SavedVariables")).unwrap();

        if sterling
        {
            fs::create_dir_all(g.join("Interface/AddOns/Sterling")).unwrap();
        }

        g
    }

    #[test]
    fn finds_flavour_folders_forever_first()
    {
        let t = tempfile::tempdir().unwrap();

        fake_install(t.path(), "_retail_", false);

        let forever = fake_install(t.path(), "_forever_", true);
        let found = discover_in(&[t.path().join("World of Warcraft"), forever.clone()]);

        assert_eq!(found.len(), 2, "no duplicates");
        assert_eq!(found[0].flavour, "_forever_");
        assert!(found[0].has_sterling && !found[0].has_companion);
        assert_eq!(found[0].accounts, vec!["TESTACCOUNT".to_string()]);
    }

    #[test]
    fn picking_a_parent_or_child_folder_still_works()
    {
        let t = tempfile::tempdir().unwrap();
        let g = fake_install(t.path(), "_forever_", true);

        assert_eq!(normalise_pick(&t.path().join("World of Warcraft")), Some(g.clone()));
        assert_eq!(normalise_pick(&g.join("Interface/AddOns/Questie")), Some(g.clone()));
        assert_eq!(normalise_pick(&g), Some(g.clone()));
        assert_eq!(normalise_pick(t.path()), None);
    }

    #[test]
    fn file_paths_are_where_the_game_keeps_them()
    {
        let g = Path::new("/g/_forever_");

        assert!(outbox_file(g, "A").ends_with("WTF/Account/A/SavedVariables/SterlingQuestSync.lua"));
        assert!(inbox_file(g).ends_with("Interface/AddOns/SterlingQuestSync/Inbox.lua"));
    }
}
