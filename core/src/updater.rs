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

pub fn valid_version(v: &str) -> bool
{
    RE_VERSION.is_match(v)
}

/// Compares dotted versions numerically ("0.2.30" > "0.2.9").
pub fn newer(a: &str, b: &str) -> bool
{
    let p = |s: &str| s.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (mut x, mut y) = (p(a), p(b));
    let n = x.len().max(y.len());

    x.resize(n, 0);
    y.resize(n, 0);
    x > y
}

/// The installed version, from `## Version:` in Sterling.toc.
pub fn installed_version(game: &Path) -> Option<String>
{
    let toc = fs::read_to_string(game.join("Interface/AddOns/Sterling/Sterling.toc")).ok()?;

    toc.lines().find_map(|l| l.strip_prefix("## Version:").map(|v| v.trim().trim_start_matches('v').to_string())).filter(|v| valid_version(v))
}

fn github() -> Result<reqwest::blocking::Client>
{
    // Release downloads redirect to GitHub's file host; nothing else is followed.
    let policy = reqwest::redirect::Policy::custom(|a|
    {
        let ok = a.url().scheme() == "https"
            && a.url().host_str().map(|h| h == "github.com" || h.ends_with(".github.com") || h.ends_with(".githubusercontent.com")).unwrap_or(false);

        if ok && a.previous().len() < 5 { a.follow() } else { a.stop() }
    });

    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(crate::api::user_agent())
        .redirect(policy)
        .build()
        .map_err(|e| Error::Server(format!("network setup failed: {e}")))
}

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

    let r = github()?
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| Error::Offline)?;

    if !r.status().is_success()
    {
        return Err(Error::Server(format!("GitHub said {}", r.status().as_u16())));
    }

    let rel: Rel = r.json().map_err(|_| Error::Server("unexpected reply from GitHub".into()))?;
    let version = rel.tag_name.trim_start_matches('v').to_string();

    if rel.draft || rel.prerelease || !valid_version(&version)
    {
        return Err(Error::Server("no usable release".into()));
    }

    let want = format!("Sterling-{version}.zip");
    let a = rel.assets.into_iter().find(|a| a.name == want).ok_or_else(|| Error::Server(format!("release has no {want}")))?;
    let prefix = format!("https://github.com/{REPO}/releases/download/");

    if !a.browser_download_url.starts_with(&prefix) || a.size > MAX_ZIP
    {
        return Err(Error::Refused("release file isn't from the Sterling repo".into()));
    }

    Ok(Release { version, url: a.browser_download_url, size: a.size })
}

/// Newest published version of a repo's releases (tag like v0.1.3), or None if there isn't one.
pub fn latest_tag(repo: &str) -> Option<String>
{
    #[derive(Deserialize)]
    struct Rel
    {
        tag_name: String,
    }

    let r = github().ok()?.get(format!("https://api.github.com/repos/{repo}/releases/latest")).header("Accept", "application/vnd.github+json").send().ok()?;

    if !r.status().is_success()
    {
        return None;
    }

    let v = r.json::<Rel>().ok()?.tag_name.trim_start_matches('v').to_string();

    valid_version(&v).then_some(v)
}

pub fn download(rel: &Release) -> Result<Vec<u8>>
{
    let r = github()?.get(&rel.url).send().map_err(|_| Error::Offline)?;

    if !r.status().is_success()
    {
        return Err(Error::Server(format!("download failed ({})", r.status().as_u16())));
    }

    let mut buf = Vec::new();

    r.take(MAX_ZIP + 1).read_to_end(&mut buf)?;

    if buf.len() as u64 > MAX_ZIP
    {
        return Err(Error::Refused("download too large".into()));
    }

    Ok(buf)
}

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

    let Some(p) = enclosed else { return bad() };
    let mut parts = Vec::new();

    for c in p.components()
    {
        match c
        {
            Component::Normal(s) =>
            {
                let s = s.to_string_lossy();

                if !RE_FILE.is_match(&s) || s.starts_with('.') && s != ".pkgmeta"
                {
                    return bad();
                }

                // Windows quietly drops a trailing dot or space, and treats some names as devices.
                let lower = s.to_ascii_lowercase();
                let stem = lower.split('.').next().unwrap_or("");
                let device = matches!(stem, "con" | "prn" | "aux" | "nul")
                    || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit());
                // An add-on is data and Lua. Nothing a double-click could run belongs in it.
                let runnable =
                    [".exe", ".dll", ".bat", ".cmd", ".com", ".scr", ".msi", ".ps1", ".vbs", ".js", ".jse", ".wsf", ".hta", ".lnk", ".url", ".jar", ".reg"]
                        .iter()
                        .any(|e| lower.ends_with(e));

                if s.ends_with('.') || s.ends_with(' ') || device || runnable
                {
                    return bad();
                }

                parts.push(s.into_owned());
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
    let mut z = zip::ZipArchive::new(Cursor::new(zip_bytes)).map_err(|_| Error::Refused("not a zip file".into()))?;

    if z.len() > MAX_ENTRIES
    {
        return Err(Error::Refused("zip has too many files".into()));
    }

    // First pass: check everything before writing anything.
    let mut total = 0u64;
    let mut has_toc = false;

    for i in 0..z.len()
    {
        let f = z.by_index(i).map_err(|_| Error::Refused("damaged zip".into()))?;

        if f.is_symlink() || f.unix_mode().map(|m| m & 0o170000 == 0o120000).unwrap_or(false)
        {
            return Err(Error::Refused("zip contains a link".into()));
        }

        let name = f.name().map_err(|_| Error::Refused("bad name in the zip".into()))?.into_owned();
        let rel = entry_path(&name, f.enclosed_name())?;

        total = total.saturating_add(f.size());

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

    for i in 0..z.len()
    {
        let mut f = z.by_index(i).map_err(|_| Error::Refused("damaged zip".into()))?;
        let name = f.name().map_err(|_| Error::Refused("bad name in the zip".into()))?.into_owned();
        let Some(rel) = entry_path(&name, f.enclosed_name())? else { continue };
        let dst = staging.join(&rel);

        if f.is_dir()
        {
            fs_.create_dir_all(&dst)?;
            continue;
        }

        let mut buf = Vec::new();
        (&mut f).take(MAX_UNPACKED - written + 1).read_to_end(&mut buf)?;
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

fn old_copies(fs_: &SafeFs) -> Vec<PathBuf>
{
    let mut v: Vec<PathBuf> = fs::read_dir(fs_.root(Root::Companion).join("_old")).into_iter().flatten().flatten().map(|e| e.path()).collect();

    v.sort();
    v.reverse();
    v
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

    if let Err(e) = fs_.rename(&staged, &live)
    {
        if had_old
        {
            let _ = fs_.rename(&aside, &live);
        }

        return Err(e);
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
            let mut w = zip::ZipWriter::new(&mut out);

            for (n, b) in entries
            {
                if n.ends_with('/')
                {
                    w.add_directory(*n, SimpleFileOptions::default()).unwrap();
                }

                else
                {
                    w.start_file(*n, SimpleFileOptions::default()).unwrap();
                    w.write_all(b).unwrap();
                }
            }

            w.finish().unwrap();
        }

        out.into_inner()
    }

    fn setup() -> (tempfile::TempDir, SafeFs)
    {
        let t = tempfile::tempdir().unwrap();
        let g = t.path().join("_forever_");

        fs::create_dir_all(g.join("Interface/AddOns/Questie")).unwrap();
        fs::write(g.join("Interface/AddOns/Questie/Questie.toc"), "q").unwrap();
        fs::create_dir_all(g.join("Interface/AddOns/Sterling")).unwrap();
        fs::write(g.join("Interface/AddOns/Sterling/Sterling.toc"), "## Version: 0.2.30\n").unwrap();
        fs::create_dir_all(g.join("WTF/Account/A/SavedVariables")).unwrap();
        fs::create_dir_all(g.join("Interface/AddOns/SterlingQuestSync")).unwrap();

        let s = SafeFs::new(&g, &t.path().join("appdata")).unwrap();
        (t, s)
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
        let (_t, s) = setup();
        let z = make_zip(&[("Sterling/", b""), ("Sterling/Sterling.toc", b"## Version: 0.2.31\n"), ("Sterling/Core/Main.lua", b"-- main")]);

        install(&s, &z, "0.2.31").unwrap();
        assert_eq!(installed_version(s.game()).as_deref(), Some("0.2.31"));
        assert!(s.game().join("Interface/AddOns/Sterling/Core/Main.lua").is_file());

        let old = old_copies(&s);

        assert_eq!(old.len(), 1);
        assert_eq!(fs::read_to_string(old[0].join("Sterling/Sterling.toc")).unwrap(), "## Version: 0.2.30\n");
        assert!(!s.root(Root::Companion).join("_staging").exists());
        assert_eq!(fs::read_to_string(s.game().join("Interface/AddOns/Questie/Questie.toc")).unwrap(), "q");
    }

    #[test]
    fn refuses_unsafe_zips_and_changes_nothing()
    {
        let (_t, s) = setup();
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

        for z in cases
        {
            assert!(install(&s, &z, "0.2.31").is_err());
            assert_eq!(installed_version(s.game()).as_deref(), Some("0.2.30"));
            assert_eq!(fs::read_to_string(s.game().join("Interface/AddOns/Questie/Questie.toc")).unwrap(), "q");
        }

        assert!(install(&s, &make_zip(&[("Sterling/Sterling.toc", b"x")]), "../../WTF").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_in_the_zip()
    {
        let (_t, s) = setup();
        let mut out = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut out);

            w.start_file("Sterling/Sterling.toc", SimpleFileOptions::default()).unwrap();
            w.write_all(b"x").unwrap();
            w.add_symlink("Sterling/WTF", "../../../WTF", SimpleFileOptions::default()).unwrap();
            w.finish().unwrap();
        }

        assert!(install(&s, &out.into_inner(), "0.2.31").is_err());
    }

    #[test]
    fn keeps_only_a_few_old_copies()
    {
        let (_t, s) = setup();

        for i in 31..37
        {
            let v = format!("0.2.{i}");
            let toc = format!("## Version: {v}\n");

            install(&s, &make_zip(&[("Sterling/Sterling.toc", toc.as_bytes())]), &v).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(old_copies(&s).len() <= KEEP_OLD);
        assert_eq!(installed_version(s.game()).as_deref(), Some("0.2.36"));
    }
}
