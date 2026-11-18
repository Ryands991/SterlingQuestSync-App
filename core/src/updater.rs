//! Keeps the Sterling Quest Sync add-on up to date from its GitHub releases.
//!
//! Only `Sterling-<version>.zip` from the add-on's own repo. The zip is checked entry by entry (no `..`,
//! no absolute paths, no links, only a top `Sterling/` folder, size caps), staged inside the
//! companion folder, then swapped in with two quick renames on the same drive. The old copy is
//! moved aside (never deleted) and kept for a few updates.

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::safety::{Root, SafeFs};
use crate::{Error, Result};

pub const REPO: &str = "SterlingAddons/SterlingQuest";
pub const APP_REPO: &str = "SterlingAddons/SterlingQuestSync-App";
const MAX_ZIP: u64 = 20 * 1024 * 1024;
const MAX_UNPACKED: u64 = 60 * 1024 * 1024;
const MAX_ENTRIES: usize = 3000;
const KEEP_OLD: usize = 3;

static RE_VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,4}\.\d{1,4}(\.\d{1,4})?$").unwrap());
static RE_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9 _.()+-]{1,80}$").unwrap());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release
{
    pub version: String,
    pub url: String,
    pub size: u64,
}

/// A version like 0.2.31.
pub fn valid_version(version: &str) -> bool
{
    RE_VERSION.is_match(version)
}

/// Compares dotted versions numerically ("0.2.30" > "0.2.9").
pub fn newer(left: &str, right: &str) -> bool
{
    let parts = |s: &str| s.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (mut left, mut right) = (parts(left), parts(right));
    let count = left.len().max(right.len());

    left.resize(count, 0);
    right.resize(count, 0);
    left > right
}

/// The installed version, from `## Version:` in Sterling.toc.
pub fn installed_version(game: &Path) -> Option<String>
{
    let toc = fs::read_to_string(game.join("Interface/AddOns/Sterling/Sterling.toc")).ok()?;

    toc.lines().find_map(|l| l.strip_prefix("## Version:").map(|v| v.trim().trim_start_matches('v').to_string())).filter(|v| valid_version(v))
}

/// The HTTP client for GitHub's release API.
fn github() -> Result<reqwest::blocking::Client>
{
    // Release downloads redirect to GitHub's file host; nothing else is followed.
    let policy = reqwest::redirect::Policy::custom(|attempt|
    {
        let ok = attempt.url().scheme() == "https"
            && attempt.url().host_str().map(|h| h == "github.com" || h.ends_with(".github.com") || h.ends_with(".githubusercontent.com")).unwrap_or(false);

        if ok && attempt.previous().len() < 5 { attempt.follow() } else { attempt.stop() }
    });

    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(crate::api::user_agent())
        .redirect(policy)
        .build()
        .map_err(|e| Error::Server(format!("network setup failed: {e}")))
}

/// Sterling Quest Sync's newest release and its zip.
pub fn latest_release() -> Result<Release>
{
    #[derive(Deserialize)]
    struct Asset
    {
        name: String,
        browser_download_url: String,
        size: u64,
    }

    #[derive(Deserialize)]
    struct Rel
    {
        tag_name: String,
        assets: Vec<Asset>,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }

    let response = github()?
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| Error::Offline)?;

    if !response.status().is_success()
    {
        return Err(Error::Server(format!("GitHub said {}", response.status().as_u16())));
    }

    let rel: Rel = response.json().map_err(|_| Error::Server("unexpected reply from GitHub".into()))?;
    let version = rel.tag_name.trim_start_matches('v').to_string();

    if rel.draft || rel.prerelease || !valid_version(&version)
    {
        return Err(Error::Server("no usable release".into()));
    }

    let want = format!("Sterling-{version}.zip");
    let asset = rel.assets.into_iter().find(|a| a.name == want).ok_or_else(|| Error::Server(format!("release has no {want}")))?;
    let prefix = format!("https://github.com/{REPO}/releases/download/");

    if !asset.browser_download_url.starts_with(&prefix) || asset.size > MAX_ZIP
    {
        return Err(Error::Refused("release file isn't from the Sterling repo".into()));
    }

    Ok(Release { version, url: asset.browser_download_url, size: asset.size })
}

/// Newest published version of a repo's releases (tag like v0.1.3), or None if there isn't one.
pub fn latest_tag(repo: &str) -> Option<String>
{
    #[derive(Deserialize)]
    struct Rel
    {
        tag_name: String,
    }

    let response =
        github().ok()?.get(format!("https://api.github.com/repos/{repo}/releases/latest")).header("Accept", "application/vnd.github+json").send().ok()?;

    if !response.status().is_success()
    {
        return None;
    }

    let version = response.json::<Rel>().ok()?.tag_name.trim_start_matches('v').to_string();

    valid_version(&version).then_some(version)
}

/// Downloads a release zip, within the size cap.
pub fn download(rel: &Release) -> Result<Vec<u8>>
{
    let response = github()?.get(&rel.url).send().map_err(|_| Error::Offline)?;

    if !response.status().is_success()
    {
        return Err(Error::Server(format!("download failed ({})", response.status().as_u16())));
    }

    let mut buf = Vec::new();

    response.take(MAX_ZIP + 1).read_to_end(&mut buf)?;

    if buf.len() as u64 > MAX_ZIP
    {
        return Err(Error::Refused("download too large".into()));
    }

    Ok(buf)
}

/// Unix time in seconds.
fn now() -> u64
{
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Checks one zip entry name and returns its path under `Sterling/`, or refuses.
fn entry_path(name: &str, enclosed: Option<PathBuf>) -> Result<Option<PathBuf>>
{
    let bad = || Err(Error::Refused(format!("unsafe entry in the zip: {name}")));

    if name.contains('\\') || name.starts_with('/') || name.contains(':')
    {
        return bad();
    }

    let Some(path) = enclosed else { return bad() };
    let mut parts = Vec::new();

    for part in path.components()
    {
        match part
        {
            Component::Normal(name) =>
            {
                let name = name.to_string_lossy();

                if !RE_FILE.is_match(&name) || name.starts_with('.') && name != ".pkgmeta"
                {
                    return bad();
                }

                // Windows quietly drops a trailing dot or space, and treats some names as devices.
                let lower = name.to_ascii_lowercase();
                let stem = lower.split('.').next().unwrap_or("");
                let device = matches!(stem, "con" | "prn" | "aux" | "nul")
                    || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit());
                // An add-on is data and Lua. Nothing a double-click could run belongs in it.
                let runnable =
                    [".exe", ".dll", ".bat", ".cmd", ".com", ".scr", ".msi", ".ps1", ".vbs", ".js", ".jse", ".wsf", ".hta", ".lnk", ".url", ".jar", ".reg"]
                        .iter()
                        .any(|e| lower.ends_with(e));

                if name.ends_with('.') || name.ends_with(' ') || device || runnable
                {
                    return bad();
                }

                parts.push(name.into_owned());
            }

            _ => return bad(),
        }
    }

    match parts.first().map(String::as_str)
    {
        Some("Sterling") if parts.len() == 1 => Ok(None),
        Some("Sterling") => Ok(Some(parts.iter().collect())),
        _ => bad(),
    }
}

/// Unpacks a Sterling zip into the staging folder after checking every entry.
fn stage(fs_: &SafeFs, zip_bytes: &[u8], version: &str) -> Result<PathBuf>
{
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes)).map_err(|_| Error::Refused("not a zip file".into()))?;

    if archive.len() > MAX_ENTRIES
    {
        return Err(Error::Refused("zip has too many files".into()));
    }

    // First pass: check everything before writing anything.
    let mut total = 0u64;
    let mut has_toc = false;

    for i in 0..archive.len()
    {
        let file = archive.by_index(i).map_err(|_| Error::Refused("damaged zip".into()))?;

        if file.is_symlink() || file.unix_mode().map(|m| m & 0o170000 == 0o120000).unwrap_or(false)
        {
            return Err(Error::Refused("zip contains a link".into()));
        }

        let name = file.name().map_err(|_| Error::Refused("bad name in the zip".into()))?.into_owned();
        let rel = entry_path(&name, file.enclosed_name())?;

        total = total.saturating_add(file.size());

        if rel.as_deref() == Some(Path::new("Sterling/Sterling.toc"))
        {
            has_toc = true;
        }
    }

    if total > MAX_UNPACKED
    {
        return Err(Error::Refused("zip unpacks too large".into()));
    }

    if !has_toc
    {
        return Err(Error::Refused("zip has no Sterling/Sterling.toc".into()));
    }

    let staging = fs_.root(Root::Companion).join("_staging").join(version);

    fs_.remove_scratch(&staging)?;

    let mut written = 0u64;

    for i in 0..archive.len()
    {
        let mut file = archive.by_index(i).map_err(|_| Error::Refused("damaged zip".into()))?;
        let name = file.name().map_err(|_| Error::Refused("bad name in the zip".into()))?.into_owned();
        let Some(rel) = entry_path(&name, file.enclosed_name())? else { continue };
        let dst = staging.join(&rel);

        if file.is_dir()
        {
            fs_.create_dir_all(&dst)?;
            continue;
        }

        let mut buf = Vec::new();
        (&mut file).take(MAX_UNPACKED - written + 1).read_to_end(&mut buf)?;
        written += buf.len() as u64;

        if written > MAX_UNPACKED
        {
            fs_.remove_scratch(&staging)?;

            return Err(Error::Refused("zip unpacks too large".into()));
        }

        fs_.write_atomic(&dst, &buf)?;
    }

    Ok(staging.join("Sterling"))
}

/// Earlier add-on copies kept in _old, oldest first.
fn old_copies(fs_: &SafeFs) -> Vec<PathBuf>
{
    let mut old: Vec<PathBuf> = fs::read_dir(fs_.root(Root::Companion).join("_old")).into_iter().flatten().flatten().map(|e| e.path()).collect();

    old.sort();
    old.reverse();
    old
}

/// Installs a downloaded Sterling zip. Safe while the game is open; Sterling then needs a /reload.
pub fn install(fs_: &SafeFs, zip_bytes: &[u8], version: &str) -> Result<()>
{
    if !valid_version(version)
    {
        return Err(Error::Refused("bad version".into()));
    }

    let staged = stage(fs_, zip_bytes, version)?;
    let live = fs_.root(Root::Sterling).to_path_buf();
    let aside = fs_.root(Root::Companion).join("_old").join(format!("{}-{}", now(), version)).join("Sterling");
    let had_old = live.exists();

    if had_old
    {
        fs_.rename(&live, &aside)?;
    }

    if let Err(error) = fs_.rename(&staged, &live)
    {
        if had_old
        {
            let _ = fs_.rename(&aside, &live);
        }

        return Err(error);
    }

    fs_.remove_scratch(&fs_.root(Root::Companion).join("_staging"))?;

    for extra in old_copies(fs_).into_iter().skip(KEEP_OLD)
    {
        fs_.remove_scratch(&extra)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests
{
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8>
    {
        let mut out = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut out);

            for (name, bytes) in entries
            {
                if name.ends_with('/')
                {
                    writer.add_directory(*name, SimpleFileOptions::default()).unwrap();
                }

                else
                {
                    writer.start_file(*name, SimpleFileOptions::default()).unwrap();
                    writer.write_all(bytes).unwrap();
                }
            }

            writer.finish().unwrap();
        }

        out.into_inner()
    }

    fn setup() -> (tempfile::TempDir, SafeFs)
    {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("_forever_");

        fs::create_dir_all(game.join("Interface/AddOns/Questie")).unwrap();
        fs::write(game.join("Interface/AddOns/Questie/Questie.toc"), "q").unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/Sterling")).unwrap();
        fs::write(game.join("Interface/AddOns/Sterling/Sterling.toc"), "## Version: 0.2.30\n").unwrap();
        fs::create_dir_all(game.join("WTF/Account/A/SavedVariables")).unwrap();
        fs::create_dir_all(game.join("Interface/AddOns/SterlingQuestSync")).unwrap();

        let safe = SafeFs::new(&game, &temp.path().join("appdata")).unwrap();
        (temp, safe)
    }

    #[test]
    fn versions_compare_numerically()
    {
        assert!(newer("0.2.30", "0.2.9"));
        assert!(newer("0.3", "0.2.99"));
        assert!(!newer("0.2.30", "0.2.30"));
        assert!(valid_version("0.2.31") && !valid_version("0.2.31; rm") && !valid_version("../x"));
    }

    #[test]
    fn installs_and_keeps_the_old_copy()
    {
        let (_t, safe) = setup();
        let zip = make_zip(&[("Sterling/", b""), ("Sterling/Sterling.toc", b"## Version: 0.2.31\n"), ("Sterling/Core/Main.lua", b"-- main")]);

        install(&safe, &zip, "0.2.31").unwrap();
        assert_eq!(installed_version(safe.game()).as_deref(), Some("0.2.31"));
        assert!(safe.game().join("Interface/AddOns/Sterling/Core/Main.lua").is_file());

        let old = old_copies(&safe);

        assert_eq!(old.len(), 1);
        assert_eq!(fs::read_to_string(old[0].join("Sterling/Sterling.toc")).unwrap(), "## Version: 0.2.30\n");
        assert!(!safe.root(Root::Companion).join("_staging").exists());
        assert_eq!(fs::read_to_string(safe.game().join("Interface/AddOns/Questie/Questie.toc")).unwrap(), "q");
    }

    #[test]
    fn refuses_unsafe_zips_and_changes_nothing()
    {
        let (_t, safe) = setup();
        let cases: Vec<Vec<u8>> = vec!
        [
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/../Questie/Questie.toc", b"pwned")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Questie/Questie.toc", b"pwned")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("/etc/passwd", b"pwned")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling\\..\\..\\WTF\\x.lua", b"pwned")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/C:/x", b"pwned")]),
            make_zip(&[("Sterling/Main.lua", b"no toc")]),
            // Audit: names Windows treats specially, and anything a double-click could run.
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/Sterling.toc.", b"shadow")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/Core.lua ", b"shadow")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/NUL", b"device")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/com1.lua", b"device")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/Readme.EXE", b"MZ")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/Media/open me.lnk", b"L")]),
            make_zip(&[("Sterling/Sterling.toc", b"x"), ("Sterling/fix.ps1", b"x")]),
            b"not a zip".to_vec(),
        ];

        for case in cases
        {
            assert!(install(&safe, &case, "0.2.31").is_err());
            assert_eq!(installed_version(safe.game()).as_deref(), Some("0.2.30"));
            assert_eq!(fs::read_to_string(safe.game().join("Interface/AddOns/Questie/Questie.toc")).unwrap(), "q");
        }

        assert!(install(&safe, &make_zip(&[("Sterling/Sterling.toc", b"x")]), "../../WTF").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_in_the_zip()
    {
        let (_t, safe) = setup();
        let mut out = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut out);

            writer.start_file("Sterling/Sterling.toc", SimpleFileOptions::default()).unwrap();
            writer.write_all(b"x").unwrap();
            writer.add_symlink("Sterling/WTF", "../../../WTF", SimpleFileOptions::default()).unwrap();
            writer.finish().unwrap();
        }

        assert!(install(&safe, &out.into_inner(), "0.2.31").is_err());
    }

    #[test]
    fn keeps_only_a_few_old_copies()
    {
        let (_t, safe) = setup();

        for i in 31..37
        {
            let version = format!("0.2.{i}");
            let toc = format!("## Version: {version}\n");

            install(&safe, &make_zip(&[("Sterling/Sterling.toc", toc.as_bytes())]), &version).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(old_copies(&safe).len() <= KEEP_OLD);
        assert_eq!(installed_version(safe.game()).as_deref(), Some("0.2.36"));
    }
}
