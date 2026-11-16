//! The sync server's API. The `Api` trait is what the engine uses, so tests can run against a fake.
//!
//! Your identity is only the key, sent as a bearer token. Keys and packet bodies are never logged.

use std::time::Duration;

use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Error, Result};

pub const DEFAULT_SERVER: &str = "https://sync.sterlingaddons.com";
pub const API_VERSION: i64 = 1;

/// A new random 256-bit key, base64url without padding (43 characters).
pub fn new_key() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn valid_key(k: &str) -> bool {
    k.len() == 43 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Only https, except a local test server.
pub fn check_server_url(u: &str) -> Result<String> {
    let u = u.trim().trim_end_matches('/');
    let (scheme, rest) = u.split_once("://").unwrap_or(("", ""));
    let host_port = rest.split('/').next().unwrap_or("");
    let host = host_port.rsplit_once(':').filter(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host_port);
    // Plain http only for a test server on this very PC. The host must match exactly:
    // "http://127.0.0.1.example.com" is somebody else's server and would send the key unencrypted.
    let ok = scheme == "https" || (scheme == "http" && (host == "127.0.0.1" || host == "localhost"));
    let host_ok = !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !rest.contains(['@', ' ', '?', '#', '\\']);
    if !ok || !host_ok {
        return Err(Error::Refused("the server address must start with https://".into()));
    }
    Ok(u.to_string())
}

// ------------------------------------------------------------------ shapes

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Meta {
    pub api: i64,
    pub min_app: String,
    pub signups: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Joined {
    pub account_id: String,
    pub key_id: String,
    #[serde(default)]
    pub recovery_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Pc {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub last_seen: i64,
    pub current: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Character {
    pub guid: String,
    pub name: String,
    pub realm: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountInfo {
    pub pcs: Vec<Pc>,
    pub characters: Vec<Character>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Code {
    pub code: String,
    pub expires_at: i64,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub guid: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenCode {
    pub id: String,
    pub guid: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Peek {
    pub partner: Character,
    pub already_paired: bool,
    pub different_install: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Redeemed {
    pub pairing_id: String,
    #[serde(default)]
    pub already_paired: bool,
    #[serde(default)]
    pub replaced: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Pairing {
    pub id: String,
    pub created_at: i64,
    pub ended_at: Option<i64>,
    pub ended_reason: Option<String>,
    pub my_guid: String,
    pub partner_guid: String,
    pub partner_name: String,
    pub partner_realm: String,
    pub my_key: Option<String>,
    pub partner_key: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Upload {
    Saved(i64),
    Unchanged(i64),
    /// The server already holds a newer save.
    Stale { seq: i64, game_time: i64 },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ServerPacket {
    pub from_guid: String,
    pub to_guid: String,
    pub packet: String,
    pub game_time: i64,
    pub proto: i64,
    pub seq: i64,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InboxPage {
    pub packets: Vec<ServerPacket>,
    pub next: i64,
    pub more: bool,
}

// ------------------------------------------------------------------ trait

/// What the engine needs from a server.
pub trait Api {
    fn put_character(&self, c: &Character) -> Result<()>;
    fn pairings(&self) -> Result<Vec<Pairing>>;
    fn end_pairing(&self, id: &str) -> Result<()>;
    fn put_slot(&self, from: &str, to: &str, packet: &str, game_time: i64, proto: i64) -> Result<Upload>;
    fn inbox(&self, since: i64) -> Result<InboxPage>;
    // Auto-pairing uses the same single-use codes as "Pair a friend".
    fn pair_code(&self, guid: &str) -> Result<Code>;
    fn cancel_code(&self, id: &str) -> Result<()>;
    fn peek_code(&self, guid: &str, code: &str) -> Result<Peek>;
    fn redeem_code(&self, guid: &str, code: &str, replace: bool) -> Result<Redeemed>;
}

// ------------------------------------------------------------------ HTTP client

pub struct Client {
    base: String,
    key: String,
    http: reqwest::blocking::Client,
}

fn http() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("SterlingQuestSync/", env!("CARGO_PKG_VERSION")))
        .https_only(false)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Server(format!("network setup failed: {e}")))
}

/// What the server's error codes mean, in words for the app.
pub fn explain(code: &str) -> &'static str {
    match code {
        "unknown_key" => "The server doesn't know this PC. It may have been removed, or the account expired.",
        "banned" => "This account has been blocked on this server.",
        "rate_limited" => "Too many tries. Wait a minute and try again.",
        "not_found" => "That code wasn't found. It may have expired or been used.",
        "own_code" => "That's your own code. Give it to your friend instead.",
        "different_install" => "You already sync with this character from a different install.",
        "partner_limit" => "This character already has as many partners as allowed.",
        "too_many_pcs" => "This account already has as many PCs as allowed.",
        "too_many_characters" => "This account already has as many characters as allowed.",
        "invite_required" => "This server needs an invite code to join.",
        "not_paired" => "You're no longer paired with that character.",
        "signups_open" => "Sign-up is open, so invites aren't needed.",
        "daily_limit" => "This account has uploaded as much as the server allows in one day. It will sync again tomorrow.",
        _ => "The server couldn't do that right now.",
    }
}

impl Client {
    pub fn new(base: &str, key: &str) -> Result<Self> {
        if !valid_key(key) {
            return Err(Error::Refused("bad key".into()));
        }
        Ok(Client { base: check_server_url(base)?, key: key.to_string(), http: http()? })
    }

    pub fn meta(base: &str) -> Result<Meta> {
        let base = check_server_url(base)?;
        let r = http()?.get(format!("{base}/v1/meta")).send().map_err(net)?;
        decode(r)
    }

    /// Makes a new account with this client's key. Returns the recovery code once.
    pub fn join(&self, label: &str, invite: Option<&str>) -> Result<Joined> {
        let mut body = json!({ "label": label });
        if let Some(i) = invite {
            body["invite"] = json!(i);
        }
        self.send("POST", "/v1/accounts", Some(body))
    }

    pub fn recover(&self, code: &str, label: &str) -> Result<Joined> {
        self.send("POST", "/v1/recover", Some(json!({ "code": code, "label": label })))
    }

    pub fn redeem_link(&self, code: &str, label: &str) -> Result<Joined> {
        self.send("POST", "/v1/link-codes/redeem", Some(json!({ "code": code, "label": label })))
    }

    pub fn account(&self) -> Result<AccountInfo> {
        self.send("GET", "/v1/account", None)
    }

    pub fn delete_account(&self) -> Result<()> {
        self.send::<Value>("DELETE", "/v1/account", None).map(|_| ())
    }

    pub fn new_recovery(&self) -> Result<String> {
        let v: Value = self.send("POST", "/v1/recovery", None)?;
        v["recovery_code"].as_str().map(String::from).ok_or_else(|| Error::Server("bad reply".into()))
    }

    pub fn remove_pc(&self, key_id: &str) -> Result<()> {
        self.send::<Value>("DELETE", &format!("/v1/keys/{}", seg(key_id)?), None).map(|_| ())
    }

    pub fn link_code(&self) -> Result<Code> {
        self.send("POST", "/v1/link-codes", None)
    }

    pub fn invite(&self) -> Result<Code> {
        self.send("POST", "/v1/invites", None)
    }

    pub fn remove_character(&self, guid: &str) -> Result<()> {
        self.send::<Value>("DELETE", &format!("/v1/characters/{}", seg(guid)?), None).map(|_| ())
    }

    pub fn pair_code(&self, guid: &str) -> Result<Code> {
        self.send("POST", "/v1/pair-codes", Some(json!({ "guid": guid })))
    }

    pub fn open_codes(&self) -> Result<Vec<OpenCode>> {
        #[derive(Deserialize)]
        struct R {
            codes: Vec<OpenCode>,
        }
        self.send::<R>("GET", "/v1/pair-codes", None).map(|r| r.codes)
    }

    pub fn cancel_code(&self, id: &str) -> Result<()> {
        self.send::<Value>("DELETE", &format!("/v1/pair-codes/{}", seg(id)?), None).map(|_| ())
    }

    pub fn peek_code(&self, guid: &str, code: &str) -> Result<Peek> {
        self.send("POST", "/v1/pair-codes/peek", Some(json!({ "guid": guid, "code": code })))
    }

    pub fn redeem_code(&self, guid: &str, code: &str, replace: bool) -> Result<Redeemed> {
        self.send("POST", "/v1/pair-codes/redeem", Some(json!({ "guid": guid, "code": code, "replace": replace })))
    }

    fn send<T: DeserializeOwned>(&self, method: &str, path: &str, body: Option<Value>) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| Error::Other("bad method".into()))?;
        let mut req = self.http.request(m, url).bearer_auth(&self.key);
        if let Some(b) = body {
            req = req.json(&b);
        }
        decode(req.send().map_err(net)?)
    }
}

/// Only IDs and GUIDs go in a URL path; anything else is refused.
fn seg(s: &str) -> Result<&str> {
    if !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        Ok(s)
    } else {
        Err(Error::Refused("bad id".into()))
    }
}

fn net(e: reqwest::Error) -> Error {
    if e.is_timeout() || e.is_connect() {
        Error::Offline
    } else {
        Error::Server("network error".into())
    }
}

fn decode<T: DeserializeOwned>(r: reqwest::blocking::Response) -> Result<T> {
    let status = r.status().as_u16();
    let text = r.text().map_err(net)?;
    if (200..300).contains(&status) {
        return serde_json::from_str(&text).map_err(|_| Error::Server("unexpected reply".into()));
    }
    let code = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"].as_str().map(String::from))
        .unwrap_or_else(|| "server_error".into());
    Err(Error::Api { status, code, body: text.chars().take(512).collect() })
}

impl Api for Client {
    fn put_character(&self, c: &Character) -> Result<()> {
        self.send::<Value>("PUT", &format!("/v1/characters/{}", seg(&c.guid)?), Some(json!({ "name": c.name, "realm": c.realm })))
            .map(|_| ())
    }

    fn pairings(&self) -> Result<Vec<Pairing>> {
        #[derive(Deserialize)]
        struct R {
            pairings: Vec<Pairing>,
        }
        self.send::<R>("GET", "/v1/pairings", None).map(|r| r.pairings)
    }

    fn end_pairing(&self, id: &str) -> Result<()> {
        self.send::<Value>("DELETE", &format!("/v1/pairings/{}", seg(id)?), None).map(|_| ())
    }

    fn put_slot(&self, from: &str, to: &str, packet: &str, game_time: i64, proto: i64) -> Result<Upload> {
        let path = format!("/v1/slots/{}/{}", seg(from)?, seg(to)?);
        match self.send::<Value>("PUT", &path, Some(json!({ "packet": packet, "game_time": game_time, "proto": proto }))) {
            Ok(v) => {
                let seq = v["seq"].as_i64().unwrap_or(0);
                Ok(if v["unchanged"].as_bool() == Some(true) { Upload::Unchanged(seq) } else { Upload::Saved(seq) })
            }
            Err(Error::Api { status: 409, code, body }) if code == "stale" => {
                let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                Ok(Upload::Stale { seq: v["seq"].as_i64().unwrap_or(0), game_time: v["game_time"].as_i64().unwrap_or(0) })
            }
            Err(e) => Err(e),
        }
    }

    fn inbox(&self, since: i64) -> Result<InboxPage> {
        self.send("GET", &format!("/v1/inbox?since={}", since.max(0)), None)
    }

    fn pair_code(&self, guid: &str) -> Result<Code> {
        Client::pair_code(self, guid)
    }

    fn cancel_code(&self, id: &str) -> Result<()> {
        Client::cancel_code(self, id)
    }

    fn peek_code(&self, guid: &str, code: &str) -> Result<Peek> {
        Client::peek_code(self, guid, code)
    }

    fn redeem_code(&self, guid: &str, code: &str, replace: bool) -> Result<Redeemed> {
        Client::redeem_code(self, guid, code, replace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_43_char_base64url() {
        let k = new_key();
        assert!(valid_key(&k), "{k}");
        assert_ne!(k, new_key());
        assert!(!valid_key("short"));
        assert!(!valid_key(&format!("{}=", &k[..42])));
    }

    #[test]
    fn server_url_must_be_https() {
        assert_eq!(check_server_url("https://sync.sterlingaddons.com/").unwrap(), "https://sync.sterlingaddons.com");
        assert!(check_server_url("http://sync.sterlingaddons.com").is_err());
        assert!(check_server_url("https://user@evil.com").is_err());
        assert!(check_server_url("ftp://x").is_err());
        assert!(check_server_url("http://127.0.0.1:8787").is_ok());
        assert!(check_server_url("http://localhost:8787").is_ok());
        // Audit: look-alike hosts must not get plain http.
        assert!(check_server_url("http://127.0.0.1.evil.example").is_err());
        assert!(check_server_url("http://localhost.evil.example:8787").is_err());
        assert!(check_server_url("http://127.0.0.1@evil.example").is_err());
        assert!(check_server_url("https://evil.example\\@sync.sterlingaddons.com").is_err());
        assert!(check_server_url("https://").is_err());
    }

    #[test]
    fn only_ids_go_in_paths() {
        assert!(seg("Player-6800-0000A11C").is_ok());
        assert!(seg("0123456789abcdef").is_ok());
        assert!(seg("../account").is_err());
        assert!(seg("a/b").is_err());
        assert!(seg("a?x=1").is_err());
        assert!(seg("").is_err());
    }
}
