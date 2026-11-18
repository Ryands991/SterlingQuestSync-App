//! Is the game running, and did it just save? Both read-only.
//!
//! Kept cheap on purpose: the process check refreshes names and paths only, and the watcher
//! listens for file events instead of polling.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::{Error, Result};

const GAME_EXES: &[&str] = &["wow.exe", "wowclassic.exe", "wow-64.exe", "wowt.exe", "wowb.exe", "world of warcraft"];

pub struct GameCheck
{
    sys: Option<System>,
}

impl Default for GameCheck
{
    fn default() -> Self
    {
        GameCheck { sys: System::new().ok() }
    }
}

impl GameCheck
{
    /// True when a WoW client is running, preferably one started from this game folder.
    pub fn running(&mut self, game: &Path) -> bool
    {
        let Some(sys) = self.sys.as_mut() else { return false };

        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet));

        let game_key = game.to_string_lossy().to_lowercase();

        sys.processes().values().any(|p|
        {
            let name = p.name().to_string_lossy().to_lowercase();
            let in_folder = p.exe().map(|e| e.to_string_lossy().to_lowercase().starts_with(&game_key)).unwrap_or(false);

            in_folder || GAME_EXES.contains(&name.as_str())
        })
    }
}

/// Add-on managers found on this PC, by friendly name: running now, or installed in their usual
/// folders. Only names are looked at; nothing of theirs is read.
pub fn addon_managers(check: &mut GameCheck) -> Vec<&'static str>
{
    let mut found: Vec<&'static str> = Vec::new();
    let mut add = |n: &'static str|
    {
        if !found.contains(&n)
        {
            found.push(n);
        }
    };

    if let Some(sys) = check.sys.as_mut()
    {
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());

        for p in sys.processes().values()
        {
            let name = p.name().to_string_lossy().to_lowercase();

            if name.starts_with("wowup")
            {
                add("WowUp");
            }

            else if name.starts_with("curseforge")
            {
                add("the CurseForge app");
            }
        }
    }

    for base in [dirs_local(), dirs_roaming()].into_iter().flatten()
    {
        for sub in [base.join("Programs"), base.clone()]
        {
            let Ok(rd) = std::fs::read_dir(&sub) else { continue };

            for e in rd.flatten()
            {
                let n = e.file_name().to_string_lossy().to_lowercase();

                if n.starts_with("wowup")
                {
                    add("WowUp");
                }

                else if n.starts_with("curseforge")
                {
                    add("the CurseForge app");
                }
            }
        }
    }

    found
}

fn dirs_local() -> Option<PathBuf>
{
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

fn dirs_roaming() -> Option<PathBuf>
{
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// Sends the account folder name whenever the companion's outbox file changes on disk.
pub struct SaveWatcher
{
    _w: RecommendedWatcher,
}

pub fn outbox_changed(path: &Path) -> bool
{
    path.file_name().map(|n| n.eq_ignore_ascii_case("SterlingQuestSync.lua")).unwrap_or(false)
}

impl SaveWatcher
{
    /// `dirs` are SavedVariables folders, watched without recursion.
    pub fn start(dirs: &[(String, PathBuf)], tx: Sender<String>) -> Result<Self>
    {
        let map: Vec<(String, PathBuf)> = dirs.to_vec();
        let lookup = map.clone();
        let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>|
        {
            let Ok(ev) = res else { return };

            if !matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_))
            {
                return;
            }

            for p in &ev.paths
            {
                if !outbox_changed(p)
                {
                    continue;
                }

                if let Some((acct, _)) = lookup.iter().find(|(_, d)| p.parent() == Some(d.as_path()))
                {
                    let _ = tx.send(acct.clone());
                }
            }
        })
        .map_err(|e| Error::Other(format!("couldn't watch for saves: {e}")))?;

        for (_, d) in &map
        {
            w.watch(d, RecursiveMode::NonRecursive).map_err(|e| Error::Other(format!("couldn't watch for saves: {e}")))?;
        }

        Ok(SaveWatcher { _w: w })
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn notices_an_outbox_save()
    {
        let t = tempfile::tempdir().unwrap();
        let sv = t.path().join("SavedVariables");

        std::fs::create_dir_all(&sv).unwrap();

        let (tx, rx) = channel();
        let _w = SaveWatcher::start(&[("ACCT".into(), sv.clone())], tx).unwrap();

        std::fs::write(sv.join("Questie.lua"), "x").unwrap();
        std::fs::write(sv.join("SterlingQuestSync.lua"), "x").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "ACCT");
    }

    #[test]
    fn no_game_running_here()
    {
        let t = tempfile::tempdir().unwrap();

        assert!(!GameCheck::default().running(t.path()));
    }
}
