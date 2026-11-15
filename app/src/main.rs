//! Sterling Quest Sync desktop app: the window and the commands it can call.
//!
//! The web page has no file or shell access. It can only call the typed commands below, and it
//! never hands the app a path: folders are picked in a native dialog on the Rust side and checked.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use sqs_core::api::{self, Api, Client};
use sqs_core::engine::{self, Report};
use sqs_core::formats::{parse_outbox, Outbox};
use sqs_core::game::{GameCheck, SaveWatcher};
use sqs_core::keystore::{KeyStore, SystemStore};
use sqs_core::paths::{self, GameFolder};
use sqs_core::safety::{write_app_file, SafeFs};
use sqs_core::{companion, updater, Error};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// Companion add-on files, bundled at build time. Inbox.lua is never bundled.
const COMPANION_FILES: &[(&str, &[u8])] = &[
    ("SterlingQuestSync.toc", include_bytes!("../companion/SterlingQuestSync.toc")),
    ("Core.lua", include_bytes!("../companion/Core.lua")),
];

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn app_data() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("SterlingQuestSync")
}

// ------------------------------------------------------------------ settings

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    game: Option<PathBuf>,
    server: String,
    pc_label: String,
    joined: bool,
    auto_sync: bool,
    auto_update_sterling: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            game: None,
            server: api::DEFAULT_SERVER.into(),
            pc_label: default_label(),
            joined: false,
            auto_sync: true,
            auto_update_sterling: false,
        }
    }
}

fn default_label() -> String {
    let h = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "My PC".into());
    clean_label(&h).unwrap_or_else(|| "My PC".into())
}

/// Labels the server accepts: letters, digits, spaces and a few marks, up to 40.
fn clean_label(s: &str) -> Option<String> {
    let t: String = s.trim().chars().filter(|c| c.is_alphanumeric() || " _.'()-".contains(*c)).take(40).collect();
    let t = t.trim().to_string();
    t.chars().next().filter(|c| c.is_alphanumeric()).map(|_| t)
}

fn load_settings() -> Settings {
    fs::read_to_string(app_data().join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_settings(s: &Settings) -> Result<(), String> {
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    write_app_file(&app_data(), "settings.json", text.as_bytes()).map_err(msg)
}

// ------------------------------------------------------------------ state

struct Session {
    fs: SafeFs,
    state: engine::State,
}

struct Inner {
    settings: Settings,
    session: Option<Session>,
    game_running: bool,
    last_report: Option<Report>,
    last_error: Option<String>,
    online: Option<bool>,
}

struct AppState {
    inner: Mutex<Inner>,
    keys: Arc<dyn KeyStore>,
    /// Restarts the save watcher after the game folder changes.
    rewatch: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

fn msg(e: Error) -> String {
    e.to_string()
}

fn open_session(game: &PathBuf) -> Result<Session, String> {
    let fs_ = SafeFs::new(game, &app_data()).map_err(msg)?;
    let state = engine::load_state(&fs_);
    Ok(Session { fs: fs_, state })
}

impl AppState {
    fn client(&self, s: &Settings) -> Result<Client, String> {
        let key = self.keys.get(&s.server).map_err(msg)?.ok_or("This PC hasn't joined yet.")?;
        Client::new(&s.server, &key).map_err(msg)
    }
}

fn read_outboxes(fs_: &SafeFs) -> (Vec<Outbox>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for acct in paths::wtf_accounts(fs_.game()) {
        match fs::read_to_string(paths::outbox_file(fs_.game(), &acct)) {
            Ok(t) => match parse_outbox(&t) {
                Ok(o) => out.push(o),
                Err(e) => problems.push(e.to_string()),
            },
            Err(_) => {}
        }
    }
    (out, problems)
}

// ------------------------------------------------------------------ status

#[derive(Serialize, Clone)]
struct CharStatus {
    guid: String,
    name: String,
    realm: String,
    saved: i64,
    ready: bool,
    partners: Vec<PartnerStatus>,
}

#[derive(Serialize, Clone)]
struct PartnerStatus {
    guid: String,
    active: bool,
    last_from_them: Option<i64>,
    delivered: bool,
}

#[derive(Serialize, Clone)]
struct Status {
    version: &'static str,
    setup_needed: bool,
    joined: bool,
    game: Option<GameFolder>,
    server: String,
    online: Option<bool>,
    pc_label: String,
    game_running: bool,
    auto_sync: bool,
    auto_update_sterling: bool,
    companion_installed: bool,
    sterling_version: Option<String>,
    last_sync: Option<i64>,
    last_report: Option<Report>,
    last_error: Option<String>,
    characters: Vec<CharStatus>,
}

fn build_status(st: &AppState) -> Status {
    let inner = st.inner.lock().unwrap();
    let s = &inner.settings;
    let mut characters = Vec::new();
    let mut companion_installed = false;
    let mut last_sync = None;
    let mut sterling_version = None;
    let mut game = None;
    if let Some(sess) = &inner.session {
        companion_installed = companion::is_installed(&sess.fs);
        last_sync = sess.state.last_sync;
        sterling_version = updater::installed_version(sess.fs.game());
        game = paths::inspect(sess.fs.game());
        let (outboxes, _) = read_outboxes(&sess.fs);
        let ready: Vec<String> = engine::pending(&sess.state, &outboxes).into_iter().map(|(g, _)| g).collect();
        for c in outboxes.iter().flat_map(|o| &o.characters) {
            let inbox = sess.state.inbox.chars.get(&c.guid);
            let partners = inbox
                .map(|ch| {
                    ch.pairs
                        .iter()
                        .map(|p| {
                            let last = ch.packets.iter().find(|x| x.from == p.partner).map(|x| (x.time, x.seq));
                            PartnerStatus {
                                guid: p.partner.clone(),
                                active: p.status == sqs_core::formats::PairStatus::Active,
                                last_from_them: last.map(|l| l.0),
                                delivered: last.is_some_and(|l| c.applied.get(&p.partner).is_some_and(|a| *a >= l.1)),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            characters.push(CharStatus {
                guid: c.guid.clone(),
                name: c.name.clone(),
                realm: c.realm.clone(),
                saved: c.saved,
                ready: ready.contains(&c.guid),
                partners,
            });
        }
    }
    Status {
        version: VERSION,
        setup_needed: inner.session.is_none() || !s.joined,
        joined: s.joined,
        game,
        server: s.server.clone(),
        online: inner.online,
        pc_label: s.pc_label.clone(),
        game_running: inner.game_running,
        auto_sync: s.auto_sync,
        auto_update_sterling: s.auto_update_sterling,
        companion_installed,
        sterling_version,
        last_sync,
        last_report: inner.last_report.clone(),
        last_error: inner.last_error.clone(),
        characters,
    }
}

fn emit_status(app: &AppHandle) {
    let st = app.state::<AppState>();
    let _ = app.emit("status", build_status(&st));
}

// ------------------------------------------------------------------ commands: setup

#[tauri::command]
fn status(st: State<'_, AppState>) -> Status {
    build_status(&st)
}

#[tauri::command(async)]
fn discover_games() -> Vec<GameFolder> {
    paths::discover()
}

fn set_game(app: &AppHandle, st: &AppState, path: PathBuf) -> Result<GameFolder, String> {
    let g = paths::normalise_pick(&path).ok_or("That folder doesn't look like a WoW game folder. Pick the one with Interface and WTF in it.")?;
    let info = paths::inspect(&g).ok_or("That folder doesn't look like a WoW game folder.")?;
    let sess = open_session(&g)?;
    companion::install(&sess.fs, COMPANION_FILES).map_err(msg)?;
    {
        let mut inner = st.inner.lock().unwrap();
        inner.settings.game = Some(g);
        save_settings(&inner.settings)?;
        inner.session = Some(sess);
    }
    if let Some(tx) = st.rewatch.lock().unwrap().as_ref() {
        let _ = tx.send(());
    }
    emit_status(app);
    Ok(info)
}

/// Uses one of the folders `discover_games` found, by its position in that list.
#[tauri::command(async)]
fn use_found_game(app: AppHandle, st: State<'_, AppState>, index: usize) -> Result<GameFolder, String> {
    let found = paths::discover();
    let g = found.get(index).ok_or("That folder isn't in the list any more.")?;
    set_game(&app, &st, g.path.clone())
}

/// Opens a folder picker on the Rust side; the page never sends a path.
#[tauri::command(async)]
fn pick_game_folder(app: AppHandle, st: State<'_, AppState>) -> Result<Option<GameFolder>, String> {
    let Some(picked) = app.dialog().file().set_title("Pick your WoW game folder (the one with Interface and WTF)").blocking_pick_folder() else {
        return Ok(None);
    };
    let p = picked.into_path().map_err(|e| e.to_string())?;
    set_game(&app, &st, p).map(Some)
}

#[derive(Serialize)]
struct Joined {
    recovery_code: Option<String>,
}

fn finish_join(app: &AppHandle, st: &AppState, server: &str, key: &str, label: &str, recovery: Option<String>) -> Result<Joined, String> {
    st.keys.set(server, key).map_err(msg)?;
    {
        let mut inner = st.inner.lock().unwrap();
        inner.settings.joined = true;
        inner.settings.pc_label = label.to_string();
        inner.settings.server = server.to_string();
        save_settings(&inner.settings)?;
    }
    emit_status(app);
    Ok(Joined { recovery_code: recovery })
}

/// "Yes, this is my first PC".
#[tauri::command(async)]
fn create_account(app: AppHandle, st: State<'_, AppState>, label: String, invite: Option<String>) -> Result<Joined, String> {
    let label = clean_label(&label).ok_or("Give this PC a name.")?;
    let server = st.inner.lock().unwrap().settings.server.clone();
    let key = api::new_key();
    let c = Client::new(&server, &key).map_err(msg)?;
    let j = c.join(&label, invite.as_deref().filter(|s| !s.trim().is_empty())).map_err(msg)?;
    finish_join(&app, &st, &server, &key, &label, j.recovery_code)
}

/// "No, I already use it on another PC".
#[tauri::command(async)]
fn link_this_pc(app: AppHandle, st: State<'_, AppState>, code: String, label: String) -> Result<Joined, String> {
    let label = clean_label(&label).ok_or("Give this PC a name.")?;
    let server = st.inner.lock().unwrap().settings.server.clone();
    let key = api::new_key();
    Client::new(&server, &key).map_err(msg)?.redeem_link(&code, &label).map_err(msg)?;
    finish_join(&app, &st, &server, &key, &label, None)
}

#[tauri::command(async)]
fn recover_account(app: AppHandle, st: State<'_, AppState>, code: String, label: String) -> Result<Joined, String> {
    let label = clean_label(&label).ok_or("Give this PC a name.")?;
    let server = st.inner.lock().unwrap().settings.server.clone();
    let key = api::new_key();
    let j = Client::new(&server, &key).map_err(msg)?.recover(&code, &label).map_err(msg)?;
    finish_join(&app, &st, &server, &key, &label, j.recovery_code)
}

/// Saves the recovery code as a .txt where the person picks, never inside the game folder.
#[tauri::command(async)]
fn save_recovery_txt(app: AppHandle, st: State<'_, AppState>, code: String) -> Result<bool, String> {
    let c = code.trim().to_uppercase();
    let ok = c.starts_with("SQS-") && c.len() <= 40 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    if !ok {
        return Err("That isn't a recovery code.".into());
    }
    let Some(p) = app.dialog().file().set_file_name("Sterling Quest Sync recovery code.txt").add_filter("Text", &["txt"]).blocking_save_file() else {
        return Ok(false);
    };
    let p = p.into_path().map_err(|e| e.to_string())?;
    let text = format!(
        "Sterling Quest Sync recovery code\r\n\r\n{c}\r\n\r\nUse it when setting up the app again if no PC is left signed in.\r\nKeep it private: it lets someone use your sync account.\r\n"
    );
    let inner = st.inner.lock().unwrap();
    match &inner.session {
        Some(sess) => sess.fs.write_user_export(&p, text.as_bytes()).map_err(msg)?,
        None => fs::write(&p, text).map_err(|e| e.to_string())?,
    }
    Ok(true)
}

// ------------------------------------------------------------------ commands: sync and pairing

fn run_sync(app: &AppHandle) -> Result<Report, String> {
    let st = app.state::<AppState>();
    let settings = st.inner.lock().unwrap().settings.clone();
    let client = st.client(&settings)?;
    let res = {
        let mut inner = st.inner.lock().unwrap();
        let sess = inner.session.as_mut().ok_or("Pick your game folder first.")?;
        let (outboxes, problems) = read_outboxes(&sess.fs);
        let mut r = engine::sync(&client, &sess.fs, &mut sess.state, &outboxes, now());
        if let Ok(rep) = r.as_mut() {
            rep.skipped.extend(problems);
        }
        r
    };
    {
        let mut inner = st.inner.lock().unwrap();
        match &res {
            Ok(r) => {
                inner.last_report = Some(r.clone());
                inner.last_error = None;
                inner.online = Some(true);
            }
            Err(Error::Offline) => {
                inner.online = Some(false);
                inner.last_error = Some("Can't reach the server. It will try again.".into());
            }
            Err(e) => inner.last_error = Some(e.to_string()),
        }
    }
    emit_status(app);
    res.map_err(msg)
}

#[tauri::command(async)]
fn sync_now(app: AppHandle) -> Result<Report, String> {
    run_sync(&app)
}

#[tauri::command]
fn set_auto_sync(app: AppHandle, st: State<'_, AppState>, on: bool) -> Result<(), String> {
    {
        let mut inner = st.inner.lock().unwrap();
        inner.settings.auto_sync = on;
        save_settings(&inner.settings)?;
    }
    emit_status(&app);
    Ok(())
}

#[tauri::command]
fn set_auto_update_sterling(app: AppHandle, st: State<'_, AppState>, on: bool) -> Result<(), String> {
    {
        let mut inner = st.inner.lock().unwrap();
        inner.settings.auto_update_sterling = on;
        save_settings(&inner.settings)?;
    }
    emit_status(&app);
    Ok(())
}

fn with_client<T>(st: &AppState, f: impl FnOnce(&Client) -> sqs_core::Result<T>) -> Result<T, String> {
    let settings = st.inner.lock().unwrap().settings.clone();
    let c = st.client(&settings)?;
    f(&c).map_err(msg)
}

#[tauri::command(async)]
fn pairings(st: State<'_, AppState>) -> Result<Vec<api::Pairing>, String> {
    with_client(&st, |c| c.pairings())
}

#[tauri::command(async)]
fn make_pair_code(st: State<'_, AppState>, guid: String) -> Result<api::Code, String> {
    with_client(&st, |c| c.pair_code(&guid))
}

#[tauri::command(async)]
fn open_codes(st: State<'_, AppState>) -> Result<Vec<api::OpenCode>, String> {
    with_client(&st, |c| c.open_codes())
}

#[tauri::command(async)]
fn cancel_code(st: State<'_, AppState>, id: String) -> Result<(), String> {
    with_client(&st, |c| c.cancel_code(&id))
}

#[tauri::command(async)]
fn peek_code(st: State<'_, AppState>, guid: String, code: String) -> Result<api::Peek, String> {
    with_client(&st, |c| c.peek_code(&guid, &code))
}

#[tauri::command(async)]
fn redeem_code(app: AppHandle, st: State<'_, AppState>, guid: String, code: String, replace: bool) -> Result<api::Redeemed, String> {
    let r = with_client(&st, |c| c.redeem_code(&guid, &code, replace))?;
    let _ = run_sync(&app);
    Ok(r)
}

#[tauri::command(async)]
fn end_pairing(app: AppHandle, st: State<'_, AppState>, id: String) -> Result<(), String> {
    with_client(&st, |c| c.end_pairing(&id))?;
    let _ = run_sync(&app);
    Ok(())
}

#[tauri::command(async)]
fn account(st: State<'_, AppState>) -> Result<api::AccountInfo, String> {
    with_client(&st, |c| c.account())
}

#[tauri::command(async)]
fn link_code(st: State<'_, AppState>) -> Result<api::Code, String> {
    with_client(&st, |c| c.link_code())
}

#[tauri::command(async)]
fn new_recovery_code(st: State<'_, AppState>) -> Result<String, String> {
    with_client(&st, |c| c.new_recovery())
}

#[tauri::command(async)]
fn remove_pc(st: State<'_, AppState>, id: String) -> Result<(), String> {
    with_client(&st, |c| c.remove_pc(&id))
}

/// "Delete my data". The confirm step lives in the page; the server does the rest.
#[tauri::command(async)]
fn delete_my_data(app: AppHandle, st: State<'_, AppState>) -> Result<(), String> {
    with_client(&st, |c| c.delete_account())?;
    {
        let mut inner = st.inner.lock().unwrap();
        let server = inner.settings.server.clone();
        st.keys.delete(&server).map_err(msg)?;
        inner.settings.joined = false;
        save_settings(&inner.settings)?;
        if let Some(sess) = inner.session.as_mut() {
            sess.state = engine::State { format: engine::STATE_FORMAT, ..Default::default() };
            engine::save_state(&sess.fs, &sess.state).map_err(msg)?;
        }
    }
    emit_status(&app);
    Ok(())
}

// ------------------------------------------------------------------ commands: Sterling updates

#[derive(Serialize)]
struct SterlingUpdate {
    installed: Option<String>,
    latest: String,
    newer: bool,
}

#[tauri::command(async)]
fn check_sterling(st: State<'_, AppState>) -> Result<SterlingUpdate, String> {
    let installed = {
        let inner = st.inner.lock().unwrap();
        inner.session.as_ref().and_then(|s| updater::installed_version(s.fs.game()))
    };
    let rel = updater::latest_release().map_err(msg)?;
    let newer = installed.as_deref().map(|i| updater::newer(&rel.version, i)).unwrap_or(true);
    Ok(SterlingUpdate { installed, latest: rel.version, newer })
}

#[tauri::command(async)]
fn update_sterling(app: AppHandle, st: State<'_, AppState>) -> Result<String, String> {
    let rel = updater::latest_release().map_err(msg)?;
    let bytes = updater::download(&rel).map_err(msg)?;
    {
        let inner = st.inner.lock().unwrap();
        let sess = inner.session.as_ref().ok_or("Pick your game folder first.")?;
        updater::install(&sess.fs, &bytes, &rel.version).map_err(msg)?;
    }
    emit_status(&app);
    Ok(rel.version)
}

/// Opens the backups folder in the file manager (the page never sees the path).
#[tauri::command]
fn open_backups() -> Result<(), String> {
    let p = app_data().join("backups");
    let _ = fs::create_dir_all(&p);
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd).arg(p).spawn().map(|_| ()).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ background

/// Watches for saves and the game closing. Cheap: one file watcher, a process check every 15 s.
fn background(app: AppHandle) {
    let (rewatch_tx, rewatch_rx) = channel::<()>();
    *app.state::<AppState>().rewatch.lock().unwrap() = Some(rewatch_tx);
    let mut game = GameCheck::default();
    loop {
        let dirs: Vec<(String, PathBuf)> = {
            let st = app.state::<AppState>();
            let inner = st.inner.lock().unwrap();
            match &inner.session {
                Some(s) => paths::wtf_accounts(s.fs.game()).into_iter().map(|a| (a.clone(), paths::saved_variables(s.fs.game(), &a))).collect(),
                None => vec![],
            }
        };
        let (tx, rx) = channel::<String>();
        let _watcher = SaveWatcher::start(&dirs, tx).ok();
        // First check at start.
        let _ = maybe_sync(&app);
        let mut next_poll = Instant::now();
        loop {
            if rewatch_rx.try_recv().is_ok() {
                break;
            }
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(_) => {
                    // The game writes several files at logout; wait for it to settle.
                    std::thread::sleep(Duration::from_secs(3));
                    while rx.try_recv().is_ok() {}
                    let _ = maybe_sync(&app);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_secs(5)),
            }
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + Duration::from_secs(15);
                let folder = {
                    let st = app.state::<AppState>();
                    let inner = st.inner.lock().unwrap();
                    inner.session.as_ref().map(|s| s.fs.game().to_path_buf())
                };
                if let Some(f) = folder {
                    let running = game.running(&f);
                    let was = {
                        let st = app.state::<AppState>();
                        let mut inner = st.inner.lock().unwrap();
                        std::mem::replace(&mut inner.game_running, running)
                    };
                    if was != running {
                        // Check the server when the game starts and right after it closes.
                        let _ = maybe_sync(&app);
                        emit_status(&app);
                    }
                }
            }
        }
    }
}

fn maybe_sync(app: &AppHandle) -> Result<(), String> {
    let ready = {
        let st = app.state::<AppState>();
        let inner = st.inner.lock().unwrap();
        inner.settings.auto_sync && inner.settings.joined && inner.session.is_some()
    };
    if ready {
        run_sync(app)?;
    } else {
        emit_status(app);
    }
    Ok(())
}

// ------------------------------------------------------------------ main

fn main() {
    let settings = load_settings();
    let session = settings.game.as_ref().and_then(|g| open_session(g).ok());
    if let Some(s) = &session {
        // Keep the companion's own files current; Inbox.lua is left alone.
        let _ = companion::install(&s.fs, COMPANION_FILES);
    }
    let state = AppState {
        inner: Mutex::new(Inner { settings, session, game_running: false, last_report: None, last_error: None, online: None }),
        keys: Arc::new(SystemStore),
        rewatch: Mutex::new(None),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(|app| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&format!("Sterling Quest Sync v{VERSION}"));
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || background(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            discover_games,
            use_found_game,
            pick_game_folder,
            create_account,
            link_this_pc,
            recover_account,
            save_recovery_txt,
            sync_now,
            set_auto_sync,
            set_auto_update_sterling,
            pairings,
            make_pair_code,
            open_codes,
            cancel_code,
            peek_code,
            redeem_code,
            end_pairing,
            account,
            link_code,
            new_recovery_code,
            remove_pc,
            delete_my_data,
            check_sterling,
            update_sterling,
            open_backups,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Sterling Quest Sync");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_cleaned() {
        assert_eq!(clean_label("  DESKTOP-KAEL  ").as_deref(), Some("DESKTOP-KAEL"));
        assert_eq!(clean_label("<script>").as_deref(), Some("script"));
        assert_eq!(clean_label("---"), None);
        assert_eq!(clean_label(&"a".repeat(60)).unwrap().len(), 40);
    }

    #[test]
    fn api_trait_is_used() {
        fn takes(_: &dyn Api) {}
        let c = Client::new("https://example.com", &api::new_key()).unwrap();
        takes(&c);
    }
}
