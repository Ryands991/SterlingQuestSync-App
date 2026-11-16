//! Sterling Quest Sync desktop app: the window and the commands it can call.
//!
//! The web page has no file or shell access. It can only call the typed commands below, and it
//! never hands the app a path: folders are picked in a native dialog on the Rust side and checked.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_updater::UpdaterExt;

use sqs_core::api::{self, Api, Client};
use sqs_core::engine::{self, Report};
use sqs_core::formats::{parse_outbox, AppInfo, CharacterOut, Kind, Look, Outbox, PartnerApp};
use sqs_core::game::{GameCheck, SaveWatcher};
use sqs_core::keystore::{KeyStore, SystemStore};
use sqs_core::paths::{self, GameFolder};
use sqs_core::safety::{write_app_file, SafeFs};
use sqs_core::{companion, updater, Error};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_ACTIVITY: usize = 200;
const STERLING_CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

// Companion add-on files, bundled at build time. Inbox.lua is never bundled.
const COMPANION_FILES: &[(&str, &[u8])] = &[
    ("SterlingQuestSync.toc", include_bytes!("../companion/SterlingQuestSync.toc")),
    ("Core.lua", include_bytes!("../companion/Core.lua")),
    ("icon.tga", include_bytes!("../companion/icon.tga")),
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
    /// Off by default: the app asks before updating itself.
    auto_update_app: bool,
    /// The X button hides the window to the tray (keeps syncing) instead of quitting.
    close_to_tray: bool,
    character: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            game: None,
            server: api::DEFAULT_SERVER.into(),
            pc_label: default_label(),
            joined: false,
            auto_sync: true,
            auto_update_sterling: true,
            auto_update_app: false,
            close_to_tray: true,
            character: None,
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

fn load_json<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    fs::read_to_string(app_data().join(name)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_json<T: Serialize>(name: &str, v: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    write_app_file(&app_data(), name, text.as_bytes()).map_err(msg)
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Activity {
    t: i64,
    text: String,
    /// "good", "bad" or "" for neutral.
    tone: String,
}

struct Session {
    fs: SafeFs,
    state: engine::State,
}

struct Inner {
    settings: Settings,
    session: Option<Session>,
    game_running: bool,
    last_error: Option<String>,
    online: Option<bool>,
    rejoin: bool,
    activity: Vec<Activity>,
    sterling_latest: Option<String>,
    app_latest: Option<String>,
    /// A signed app update the updater found, ready to install.
    app_update: Option<String>,
    sterling_checked: Option<Instant>,
    busy: bool,
    /// Folder shown in setup step 1, not used until Continue.
    candidate: Option<PathBuf>,
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

impl Inner {
    fn log(&mut self, text: impl Into<String>, tone: &str) {
        self.activity.insert(0, Activity { t: now(), text: text.into(), tone: tone.into() });
        self.activity.truncate(MAX_ACTIVITY);
        let _ = save_json("activity.json", &self.activity);
    }

    /// Display names for GUIDs: your own characters and your partners.
    fn names(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        if let Some(s) = &self.session {
            for (g, c) in &s.state.listed {
                m.insert(g.clone(), c.name.clone());
            }
            for p in &s.state.pairs {
                if !p.partner_name.is_empty() {
                    m.insert(p.partner_guid.clone(), p.partner_name.clone());
                }
            }
        }
        m
    }
}

fn read_outboxes(fs_: &SafeFs) -> (Vec<Outbox>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for acct in paths::wtf_accounts(fs_.game()) {
        if let Ok(t) = fs::read_to_string(paths::outbox_file(fs_.game(), &acct)) {
            match parse_outbox(&t) {
                Ok(o) => out.push(o),
                Err(e) => problems.push(e.to_string()),
            }
        }
    }
    (out, problems)
}

// ------------------------------------------------------------------ status

#[derive(Serialize, Clone)]
struct PartnerStatus {
    pairing_id: String,
    guid: String,
    name: String,
    realm: String,
    active: bool,
    /// "removed", "replaced", "deleted" or similar when the pairing ended.
    ended_reason: Option<String>,
    ended_at: Option<i64>,
    /// Game time of the newest copy from them on this PC.
    last_copy: Option<i64>,
    /// Sterling has applied that copy in game.
    applied: bool,
    /// Level, class and race from Sterling, for the window only.
    look: Look,
}

#[derive(Serialize, Clone)]
struct CharStatus {
    guid: String,
    name: String,
    realm: String,
    /// Game time of the last save the app has seen, if any.
    saved: Option<i64>,
    ready: bool,
    look: Look,
    /// Favourite partners, in order (up to 5).
    favs: Vec<String>,
    /// Sterling's version as this character last saved it.
    sterling: Option<String>,
    partners: Vec<PartnerStatus>,
    /// Sterling partners with no live pairing in the app yet (names only if Sterling wrote them).
    unpaired: Vec<Unpaired>,
}

#[derive(Serialize, Clone)]
struct Unpaired {
    guid: String,
    name: Option<String>,
    /// A code is in the inbox for Sterling to hand over in game.
    offered: bool,
    /// "yes", "no" or "unknown": whether their Sterling said in game that they have the app.
    app: &'static str,
    look: Look,
}

#[derive(Serialize, Clone)]
struct Status {
    version: &'static str,
    setup_needed: bool,
    joined: bool,
    rejoin: bool,
    game: Option<GameFolder>,
    game_path: Option<String>,
    online: Option<bool>,
    pc_label: String,
    game_running: bool,
    auto_sync: bool,
    auto_update_sterling: bool,
    companion_installed: bool,
    sterling_version: Option<String>,
    sterling_latest: Option<String>,
    app_latest: Option<String>,
    app_update: Option<String>,
    auto_update_app: bool,
    close_to_tray: bool,
    last_sync: Option<i64>,
    last_error: Option<String>,
    busy: bool,
    character: Option<String>,
    characters: Vec<CharStatus>,
    activity: Vec<Activity>,
}

fn build_status(st: &AppState) -> Status {
    let inner = st.inner.lock().unwrap();
    let s = &inner.settings;
    let mut characters: Vec<CharStatus> = Vec::new();
    let mut companion_installed = false;
    let mut last_sync = None;
    let mut sterling_version = None;
    let mut game = None;
    let mut game_path = None;
    if let Some(sess) = &inner.session {
        companion_installed = companion::is_installed(&sess.fs);
        last_sync = sess.state.last_sync;
        sterling_version = updater::installed_version(sess.fs.game());
        game = paths::inspect(sess.fs.game());
        game_path = Some(sess.fs.game().display().to_string());
        let (outboxes, _) = read_outboxes(&sess.fs);
        let ready: Vec<String> = engine::pending(&sess.state, &outboxes).into_iter().map(|(g, _)| g).collect();
        let saved: BTreeMap<&str, (i64, &BTreeMap<String, i64>)> =
            outboxes.iter().flat_map(|o| &o.characters).map(|c| (c.guid.as_str(), (c.saved, &c.applied))).collect();
        // Characters seen in the outbox, plus any listed on the server from another PC.
        let mut guids: Vec<(String, String, String)> =
            outboxes.iter().flat_map(|o| &o.characters).map(|c| (c.guid.clone(), c.name.clone(), c.realm.clone())).collect();
        for (g, c) in &sess.state.listed {
            if !guids.iter().any(|x| &x.0 == g) {
                guids.push((g.clone(), c.name.clone(), c.realm.clone()));
            }
        }
        for (guid, name, realm) in guids {
            let inbox = sess.state.inbox.chars.get(&guid);
            let applied = saved.get(guid.as_str()).map(|x| x.1);
            let me: Option<&CharacterOut> = outboxes.iter().flat_map(|o| &o.characters).find(|c| c.guid == guid);
            let look_of = |g: &str| me.and_then(|c| c.partners.iter().find(|p| p.guid == g)).map(|p| p.look.clone()).unwrap_or_default();
            let partners = sess
                .state
                .pairs
                .iter()
                .filter(|p| p.my_guid == guid)
                .map(|p| {
                    let last = inbox.and_then(|ch| ch.packets.iter().find(|x| x.from == p.partner_guid && x.kind == Kind::Partner));
                    PartnerStatus {
                        pairing_id: p.id.clone(),
                        guid: p.partner_guid.clone(),
                        name: p.partner_name.clone(),
                        realm: p.partner_realm.clone(),
                        active: p.active,
                        ended_reason: p.ended_reason.clone(),
                        ended_at: p.ended_at,
                        last_copy: last.map(|x| x.time),
                        applied: last.is_some_and(|x| applied.and_then(|a| a.get(&p.partner_guid)).is_some_and(|n| *n >= x.seq)),
                        look: look_of(&p.partner_guid),
                    }
                })
                .collect();
            let live: Vec<&str> = sess.state.pairs.iter().filter(|p| p.my_guid == guid && p.active).map(|p| p.partner_guid.as_str()).collect();
            let unpaired: Vec<Unpaired> = outboxes
                .iter()
                .flat_map(|o| &o.characters)
                .filter(|c| c.guid == guid)
                .flat_map(|c| &c.partners)
                .filter(|p| !p.removed && !live.contains(&p.guid.as_str()))
                .map(|p| Unpaired {
                    guid: p.guid.clone(),
                    name: p.name.clone(),
                    offered: sess.state.offers.get(&guid).is_some_and(|m| m.contains_key(&p.guid)),
                    app: match p.app {
                        PartnerApp::Has(_) => "yes",
                        PartnerApp::No => "no",
                        PartnerApp::Unknown => "unknown",
                    },
                    look: p.look.clone(),
                })
                .collect();
            characters.push(CharStatus {
                favs: sess.state.favs.get(&guid).map(|f| f.list.clone()).unwrap_or_default(),
                look: me.map(|c| c.look.clone()).unwrap_or_default(),
                sterling: me.and_then(|c| c.sterling.clone()),
                unpaired,
                ready: ready.contains(&guid),
                saved: saved.get(guid.as_str()).map(|x| x.0),
                guid,
                name,
                realm,
                partners,
            });
        }
        characters.sort_by(|a, b| b.saved.cmp(&a.saved).then(a.name.cmp(&b.name)));
    }
    Status {
        version: VERSION,
        setup_needed: inner.session.is_none() || !s.joined,
        joined: s.joined,
        rejoin: inner.rejoin,
        game,
        game_path,
        online: inner.online,
        pc_label: s.pc_label.clone(),
        game_running: inner.game_running,
        auto_sync: s.auto_sync,
        auto_update_sterling: s.auto_update_sterling,
        companion_installed,
        sterling_version,
        sterling_latest: inner.sterling_latest.clone(),
        app_latest: inner.app_latest.clone(),
        app_update: inner.app_update.clone(),
        auto_update_app: s.auto_update_app,
        close_to_tray: s.close_to_tray,
        last_sync,
        last_error: inner.last_error.clone(),
        busy: inner.busy,
        character: s.character.clone(),
        characters,
        activity: inner.activity.iter().take(60).cloned().collect(),
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
        save_json("settings.json", &inner.settings)?;
        inner.session = Some(sess);
        inner.log(format!("Using the {} game folder. Added the SterlingQuestSync add-on.", info.flavour), "");
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

/// Setup step 1: show a found folder without using it yet.
#[tauri::command(async)]
fn candidate_found(st: State<'_, AppState>, index: usize) -> Result<GameFolder, String> {
    let found = paths::discover();
    let g = found.get(index).ok_or("That folder isn't in the list any more.")?;
    st.inner.lock().unwrap().candidate = Some(g.path.clone());
    Ok(g.clone())
}

/// Setup step 1: "Change..." opens a folder picker; the folder is shown, not used yet.
#[tauri::command(async)]
fn candidate_pick(app: AppHandle, st: State<'_, AppState>) -> Result<Option<GameFolder>, String> {
    let Some(picked) = app.dialog().file().set_title("Pick your WoW game folder (the one with Interface and WTF)").blocking_pick_folder() else {
        return Ok(None);
    };
    let p = picked.into_path().map_err(|e| e.to_string())?;
    let g = paths::normalise_pick(&p).ok_or("That folder doesn't look like a WoW game folder. Pick the one with Interface and WTF in it.")?;
    let info = paths::inspect(&g).ok_or("That folder doesn't look like a WoW game folder.")?;
    st.inner.lock().unwrap().candidate = Some(g);
    Ok(Some(info))
}

/// Setup step 1 Continue: use the shown folder, and install Sterling there if asked.
#[tauri::command(async)]
fn confirm_game(app: AppHandle, st: State<'_, AppState>, install_sterling: bool) -> Result<(), String> {
    let c = st.inner.lock().unwrap().candidate.clone().ok_or("Pick a game folder first.")?;
    set_game(&app, &st, c)?;
    if install_sterling {
        install_sterling_now(&app).map_err(|e| format!("Your folder is set, but Sterling couldn't be installed: {e}. You can try again from Home."))?;
    }
    Ok(())
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

fn finish_join(app: &AppHandle, st: &AppState, server: &str, key: &str, label: &str, recovery: Option<String>, how: &str) -> Result<Joined, String> {
    st.keys.set(server, key).map_err(msg)?;
    {
        let mut inner = st.inner.lock().unwrap();
        inner.settings.joined = true;
        inner.settings.pc_label = label.to_string();
        inner.settings.server = server.to_string();
        inner.rejoin = false;
        inner.last_error = None;
        save_json("settings.json", &inner.settings)?;
        // A new account starts with a clean sync state on this PC.
        if let Some(sess) = inner.session.as_mut() {
            sess.state = engine::State { format: engine::STATE_FORMAT, ..Default::default() };
            let _ = engine::save_state(&sess.fs, &sess.state);
        }
        inner.log(how.to_string(), "good");
    }
    emit_status(app);
    let h = app.clone();
    std::thread::spawn(move || {
        let _ = run_sync(&h);
    });
    Ok(Joined { recovery_code: recovery })
}

fn server_of(st: &AppState) -> String {
    st.inner.lock().unwrap().settings.server.clone()
}

/// "Yes, this is my first PC", and "Start fresh" on the rejoin screen.
#[tauri::command(async)]
fn create_account(app: AppHandle, st: State<'_, AppState>, label: String, invite: Option<String>) -> Result<Joined, String> {
    let label = clean_label(&label).ok_or("Give this PC a name.")?;
    let server = server_of(&st);
    let key = api::new_key();
    let c = Client::new(&server, &key).map_err(msg)?;
    let j = c.join(&label, invite.as_deref().filter(|s| !s.trim().is_empty())).map_err(msg)?;
    finish_join(&app, &st, &server, &key, &label, j.recovery_code, "Made your sync account.")
}

/// "No, I already use it on another PC".
#[tauri::command(async)]
fn link_this_pc(app: AppHandle, st: State<'_, AppState>, code: String, label: String) -> Result<Joined, String> {
    let label = clean_label(&label).ok_or("Give this PC a name.")?;
    let server = server_of(&st);
    let key = api::new_key();
    Client::new(&server, &key).map_err(msg)?.redeem_link(&code, &label).map_err(msg)?;
    finish_join(&app, &st, &server, &key, &label, None, "Linked this PC to your account.")
}

#[tauri::command(async)]
fn recover_account(app: AppHandle, st: State<'_, AppState>, code: String, label: String) -> Result<Joined, String> {
    let label = clean_label(&label).ok_or("Give this PC a name.")?;
    let server = server_of(&st);
    let key = api::new_key();
    let j = Client::new(&server, &key).map_err(msg)?.recover(&code, &label).map_err(msg)?;
    finish_join(&app, &st, &server, &key, &label, j.recovery_code, "Got your account back with the recovery code.")
}

/// Saves the recovery code as a .txt where the person picks, never inside the game folder.
#[tauri::command(async)]
fn save_recovery_txt(app: AppHandle, st: State<'_, AppState>, code: String) -> Result<bool, String> {
    let c = code.trim().to_uppercase();
    let ok = c.starts_with("SQS-") && c.len() <= 40 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    if !ok {
        return Err("That isn't a recovery code.".into());
    }
    let Some(p) = app.dialog().file().set_file_name("sterling-quest-sync-recovery.txt").add_filter("Text", &["txt"]).blocking_save_file() else {
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

// ------------------------------------------------------------------ sync

fn describe(inner: &mut Inner, r: &Report) {
    let names = inner.names();
    let name = |g: &String| names.get(g).cloned().unwrap_or_else(|| "a character".into());
    let mut ups: Vec<String> = r.sent_to.iter().map(|(f, _)| name(f)).collect();
    ups.dedup();
    for u in ups {
        inner.log(format!("Uploaded {u}'s progress"), "good");
    }
    for (f, t) in &r.received_from {
        if f == t {
            inner.log(format!("Got {}'s copy from your other PC", name(f)), "good");
        } else {
            inner.log(format!("Got {}'s latest copy for {}", name(f), name(t)), "good");
        }
    }
    for (f, t) in &r.stale {
        inner.log(format!("Skipped an older save of {} for {}: the server already has a newer one", name(f), name(t)), "");
    }
    for (_, _, who) in &r.auto_paired {
        inner.log(format!("Paired with {who} automatically"), "good");
    }
    for n in &r.auto_notes {
        inner.log(n.clone(), "");
    }
    for id in &r.ended {
        let who = inner.session.as_ref().and_then(|s| s.state.pairs.iter().find(|p| &p.id == id)).map(|p| p.partner_name.clone()).unwrap_or_default();
        inner.log(format!("Stopped syncing with {} (removed in game)", if who.is_empty() { "a partner".into() } else { who }), "");
    }
    if !r.skipped.is_empty() {
        inner.log(format!("Skipped {} item(s) that didn't pass checks", r.skipped.len()), "bad");
    }
    if r.inbox_written && (!r.received_from.is_empty() || !r.ended.is_empty() || r.auto_offered > 0) {
        inner.log("Updated the inbox. /reload or log in to see it in game.", "");
    }
}

fn run_sync(app: &AppHandle) -> Result<Report, String> {
    let st = app.state::<AppState>();
    let settings = {
        let mut inner = st.inner.lock().unwrap();
        if inner.busy {
            return Err("Already syncing.".into());
        }
        inner.busy = true;
        inner.settings.clone()
    };
    emit_status(app);
    let res = (|| {
        let client = st.client(&settings).map_err(Error::Other)?;
        let mut inner = st.inner.lock().unwrap();
        // Versions the add-on shows after a /reload.
        let app_info = AppInfo { version: VERSION.into(), latest: inner.app_latest.clone(), sterling_latest: inner.sterling_latest.clone() };
        let sess = inner.session.as_mut().ok_or_else(|| Error::Other("Pick your game folder first.".into()))?;
        let (outboxes, problems) = read_outboxes(&sess.fs);
        let mut r = engine::sync(&client, &sess.fs, &mut sess.state, &outboxes, Some(app_info), now());
        if let Ok(rep) = r.as_mut() {
            rep.skipped.extend(problems);
        }
        r
    })();
    {
        let mut inner = st.inner.lock().unwrap();
        inner.busy = false;
        match &res {
            Ok(r) => {
                inner.last_error = None;
                inner.online = Some(true);
                describe(&mut inner, r);
            }
            Err(Error::Offline) => {
                if inner.online != Some(false) {
                    inner.log("Can't reach the server. It will try again.", "bad");
                }
                inner.online = Some(false);
                inner.last_error = Some("Can't reach the server. It will try again.".into());
            }
            Err(Error::Api { code, .. }) if code == "unknown_key" => {
                inner.online = Some(true);
                inner.rejoin = true;
                inner.last_error = None;
            }
            Err(e) => {
                inner.online = Some(true);
                let m = e.to_string();
                inner.log(format!("Sync failed: {m}"), "bad");
                inner.last_error = Some(m);
            }
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
fn set_options(
    app: AppHandle,
    st: State<'_, AppState>,
    auto_sync: Option<bool>,
    auto_update_sterling: Option<bool>,
    auto_update_app: Option<bool>,
    close_to_tray: Option<bool>,
    character: Option<String>,
) -> Result<(), String> {
    {
        let mut inner = st.inner.lock().unwrap();
        if let Some(v) = auto_sync {
            inner.settings.auto_sync = v;
        }
        if let Some(v) = auto_update_sterling {
            inner.settings.auto_update_sterling = v;
        }
        if let Some(v) = close_to_tray {
            inner.settings.close_to_tray = v;
        }
        if let Some(v) = auto_update_app {
            inner.settings.auto_update_app = v;
        }
        if let Some(c) = character.filter(|c| sqs_core::formats::valid_guid(c)) {
            inner.settings.character = Some(c);
        }
        save_json("settings.json", &inner.settings)?;
    }
    emit_status(&app);
    Ok(())
}

/// Star or unstar a partner of the selected character. Ok(false) means 5 are already starred.
#[tauri::command]
fn set_favorite(app: AppHandle, st: State<'_, AppState>, partner: String, on: bool) -> Result<bool, String> {
    let ok = {
        let mut inner = st.inner.lock().unwrap();
        let me = inner.settings.character.clone().ok_or("Select a character first.")?;
        let sess = inner.session.as_mut().ok_or("Pick your game folder first.")?;
        engine::set_favorite(&sess.fs, &mut sess.state, &me, &partner, on, now()).map_err(msg)?
    };
    emit_status(&app);
    Ok(ok)
}

// ------------------------------------------------------------------ commands: pairing and account

fn with_client<T>(st: &AppState, f: impl FnOnce(&Client) -> sqs_core::Result<T>) -> Result<T, String> {
    let settings = st.inner.lock().unwrap().settings.clone();
    let c = st.client(&settings)?;
    f(&c).map_err(msg)
}

/// The server lists only characters it has seen from the game. Pairing needs one.
fn ensure_listed(st: &AppState, guid: &str) -> Result<(), String> {
    let ch = {
        let inner = st.inner.lock().unwrap();
        inner.session.as_ref().and_then(|s| s.state.listed.get(guid).cloned())
    };
    match ch {
        Some(_) => Ok(()),
        None => Err("Log in on that character once with Sterling and the SterlingQuestSync add-on enabled, then try again.".into()),
    }
}

#[tauri::command(async)]
fn make_pair_code(st: State<'_, AppState>, guid: String) -> Result<api::Code, String> {
    ensure_listed(&st, &guid)?;
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
    ensure_listed(&st, &guid)?;
    with_client(&st, |c| c.peek_code(&guid, &code))
}

#[tauri::command(async)]
fn redeem_code(app: AppHandle, st: State<'_, AppState>, guid: String, code: String, replace: bool) -> Result<api::Redeemed, String> {
    let peek = with_client(&st, |c| c.peek_code(&guid, &code))?;
    let r = with_client(&st, |c| c.redeem_code(&guid, &code, replace))?;
    st.inner.lock().unwrap().log(format!("Paired with {}", peek.partner.name), "good");
    let _ = run_sync(&app);
    Ok(r)
}

#[tauri::command(async)]
fn end_pairing(app: AppHandle, st: State<'_, AppState>, id: String) -> Result<(), String> {
    with_client(&st, |c| c.end_pairing(&id))?;
    let who = {
        let inner = st.inner.lock().unwrap();
        inner.session.as_ref().and_then(|s| s.state.pairs.iter().find(|p| p.id == id)).map(|p| p.partner_name.clone()).unwrap_or_default()
    };
    st.inner.lock().unwrap().log(format!("Stopped syncing with {who}"), "");
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
    let c = with_client(&st, |c| c.new_recovery())?;
    st.inner.lock().unwrap().log("Made a new recovery code. The old one no longer works.", "");
    Ok(c)
}

#[tauri::command(async)]
fn remove_pc(st: State<'_, AppState>, id: String) -> Result<(), String> {
    with_client(&st, |c| c.remove_pc(&id))?;
    st.inner.lock().unwrap().log("Removed a PC from your account.", "");
    Ok(())
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
        save_json("settings.json", &inner.settings)?;
        if let Some(sess) = inner.session.as_mut() {
            sess.state = engine::State { format: engine::STATE_FORMAT, ..Default::default() };
            engine::save_state(&sess.fs, &sess.state).map_err(msg)?;
        }
        inner.log("Deleted your data from the server.", "");
    }
    emit_status(&app);
    Ok(())
}

// ------------------------------------------------------------------ Sterling updates

fn check_sterling_quiet(app: &AppHandle) -> Option<String> {
    let app_latest = updater::latest_tag(updater::APP_REPO);
    let st = app.state::<AppState>();
    if app_latest.is_some() {
        st.inner.lock().unwrap().app_latest = app_latest;
    }
    let rel = updater::latest_release().ok()?;
    let mut inner = st.inner.lock().unwrap();
    inner.sterling_latest = Some(rel.version.clone());
    inner.sterling_checked = Some(Instant::now());
    Some(rel.version)
}

fn install_sterling_now(app: &AppHandle) -> Result<String, String> {
    install_sterling(app)
}

fn install_sterling(app: &AppHandle) -> Result<String, String> {
    let st = app.state::<AppState>();
    let rel = updater::latest_release().map_err(msg)?;
    let bytes = updater::download(&rel).map_err(msg)?;
    {
        let mut inner = st.inner.lock().unwrap();
        let sess = inner.session.as_ref().ok_or("Pick your game folder first.")?;
        let fresh = updater::installed_version(sess.fs.game()).is_none();
        updater::install(&sess.fs, &bytes, &rel.version).map_err(msg)?;
        inner.sterling_latest = Some(rel.version.clone());
        let running = inner.game_running;
        let hint = if running && fresh { " Restart the game to load it." } else if running { " Type /reload in game to load it." } else { "" };
        inner.log(format!("{} Sterling {}.{}", if fresh { "Installed" } else { "Updated to" }, rel.version, hint), "good");
    }
    emit_status(app);
    Ok(rel.version)
}

#[tauri::command(async)]
fn update_sterling(app: AppHandle) -> Result<String, String> {
    install_sterling(&app)
}

// ------------------------------------------------------------------ app updates

/// Asks the update feed whether a newer, signed app is out. Signature checks happen on install.
fn check_app_update(app: &AppHandle) -> Option<String> {
    let found = tauri::async_runtime::block_on(async {
        let u = app.updater().ok()?;
        u.check().await.ok().flatten().map(|x| x.version)
    });
    let st = app.state::<AppState>();
    st.inner.lock().unwrap().app_update = found.clone();
    found
}

/// Downloads the update, checks its signature against the key built into this app, and runs it.
/// The installer closes the app and starts the new version.
fn install_app_update(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    st.inner.lock().unwrap().log("Updating Sterling Quest Sync...", "");
    emit_status(app);
    tauri::async_runtime::block_on(async {
        let u = app.updater().map_err(|e| e.to_string())?;
        let up = u.check().await.map_err(|e| e.to_string())?.ok_or("Already up to date.")?;
        up.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })?;
    app.restart();
}

#[tauri::command(async)]
fn update_app(app: AppHandle) -> Result<(), String> {
    install_app_update(&app).map_err(|e| {
        app.state::<AppState>().inner.lock().unwrap().log(format!("App update failed: {e}"), "bad");
        emit_status(&app);
        e
    })
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

/// Opens one of a few fixed web pages in the browser.
#[tauri::command]
fn open_page(which: String) -> Result<(), String> {
    let url = match which.as_str() {
        "repo" => "https://github.com/SterlingAddons/SterlingQuestSync-App",
        "issues" => "https://github.com/SterlingAddons/SterlingQuestSync-App/issues",
        "privacy" => "https://github.com/SterlingAddons/SterlingQuestSync-Server#privacy",
        _ => return Err("unknown page".into()),
    };
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn hide_to_tray(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// The X button: hide to the tray, or quit, as set in Settings.
#[tauri::command]
fn close_window(app: AppHandle, st: State<'_, AppState>) {
    if st.inner.lock().unwrap().settings.close_to_tray {
        hide_to_tray(app);
    } else {
        app.exit(0);
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

// ------------------------------------------------------------------ background

/// Watches for saves and the game closing. Cheap: one file watcher, a process check every 15 s.
fn background(app: AppHandle) {
    let (rewatch_tx, rewatch_rx) = channel::<()>();
    *app.state::<AppState>().rewatch.lock().unwrap() = Some(rewatch_tx);
    let mut game = GameCheck::default();
    let _ = check_sterling_quiet(&app);
    let _ = check_app_update(&app);
    emit_status(&app);
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
                let (folder, auto_sterling, checked) = {
                    let st = app.state::<AppState>();
                    let inner = st.inner.lock().unwrap();
                    (inner.session.as_ref().map(|s| s.fs.game().to_path_buf()), inner.settings.auto_update_sterling, inner.sterling_checked)
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
                    if checked.is_none_or(|c| c.elapsed() >= STERLING_CHECK_EVERY) {
                        let auto_app = app.state::<AppState>().inner.lock().unwrap().settings.auto_update_app;
                        if check_app_update(&app).is_some() && auto_app && !running {
                            let _ = install_app_update(&app);
                        }
                        if let Some(latest) = check_sterling_quiet(&app) {
                            let installed = updater::installed_version(&f);
                            if auto_sterling && installed.as_deref().is_some_and(|i| updater::newer(&latest, i)) {
                                let _ = install_sterling(&app);
                            }
                        }
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
        inner.settings.auto_sync && inner.settings.joined && inner.session.is_some() && !inner.rejoin
    };
    if ready {
        run_sync(app)?;
    } else {
        emit_status(app);
    }
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// ------------------------------------------------------------------ main

fn main() {
    let settings: Settings = fs::read_to_string(app_data().join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let session = settings.game.as_ref().and_then(|g| open_session(g).ok());
    if let Some(s) = &session {
        // Keep the companion's own files current; Inbox.lua is left alone.
        let _ = companion::install(&s.fs, COMPANION_FILES);
    }
    let state = AppState {
        inner: Mutex::new(Inner {
            settings,
            session,
            game_running: false,
            last_error: None,
            online: None,
            rejoin: false,
            activity: load_json("activity.json"),
            sterling_latest: None,
            app_latest: None,
            app_update: None,
            sterling_checked: None,
            busy: false,
            candidate: None,
        }),
        keys: Arc::new(SystemStore),
        rewatch: Mutex::new(None),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .on_window_event(|window, event| {
            // Alt+F4 and other ways of closing follow the same setting as the X button.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let to_tray = window.app_handle().state::<AppState>().inner.lock().unwrap().settings.close_to_tray;
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open Sterling Quest Sync", true, None::<&str>)?;
            let sync = MenuItem::with_id(app, "sync", "Sync now", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &sync, &quit])?;
            let mut tray = TrayIconBuilder::with_id("main").tooltip(format!("Sterling Quest Sync v{VERSION}")).menu(&menu).show_menu_on_left_click(false);
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, e| match e.id.as_ref() {
                "open" => show_main(app),
                "sync" => {
                    let h = app.clone();
                    std::thread::spawn(move || {
                        let _ = run_sync(&h);
                    });
                }
                "quit" => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(|tray, e| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    show_main(tray.app_handle());
                }
            })
            .build(app)?;
            let handle = app.handle().clone();
            std::thread::spawn(move || background(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            set_favorite,
            discover_games,
            use_found_game,
            pick_game_folder,
            candidate_found,
            candidate_pick,
            confirm_game,
            create_account,
            link_this_pc,
            recover_account,
            save_recovery_txt,
            sync_now,
            set_options,
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
            update_sterling,
            update_app,
            open_backups,
            open_page,
            hide_to_tray,
            close_window,
            quit,
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
}
