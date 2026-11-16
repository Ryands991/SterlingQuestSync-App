//! The two files shared with the game.
//!
//! Outbox (game to app): `WTF/Account/<ACCOUNT>/SavedVariables/SterlingQuestSync.lua`, written by the
//! game at logout or /reload from the companion add-on's SavedVariables `SterlingQuestSyncOut`.
//! The app only reads it, with a strict parser, and validates every field.
//!
//! Inbox (app to game): `Interface/AddOns/SterlingQuestSync/Inbox.lua`. It is Lua code, so it is
//! built from a fixed template and every value is allowlisted. No names or free text, ever.
//!
//! Format version 1. Field names here are the contract for the Sterling side of the work.

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use crate::lua::{parse_saved_variables, Table, Value};
use crate::{Error, Result};

pub const FORMAT: i64 = 1;
pub const MAX_PACKET: usize = 64 * 1024;

pub static RE_GUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Player-\d{1,5}-[0-9A-F]{8}$").unwrap());
pub static RE_PACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9()]+$").unwrap());
pub static RE_PAIR_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z2-9]{16}$").unwrap());
/// A pairing code as the server hands it out: XXXX-XXXX-XXXX from its 30-letter alphabet.
pub static RE_PAIR_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-HJKMNP-TV-Z2-9]{4}-[A-HJKMNP-TV-Z2-9]{4}-[A-HJKMNP-TV-Z2-9]{4}$").unwrap());
static RE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{L}[\p{L}' -]{0,23}$").unwrap());
static RE_REALM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}][\p{L}\p{N}' ()-]{0,63}$").unwrap());

pub fn valid_guid(s: &str) -> bool {
    RE_GUID.is_match(s)
}

pub fn valid_packet(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_PACKET && RE_PACKET.is_match(s)
}

// ------------------------------------------------------------------ outbox

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartnerOut {
    pub guid: String,
    /// Full copy for this partner, or None when the partner was removed in game.
    pub packet: Option<String>,
    /// Change number for the partner entry (two-way edits, remove wins).
    pub change: i64,
    pub removed: bool,
    /// Display name, if Sterling wrote one. Shown only in this PC's window; never uploaded.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether this partner has Sterling Quest Sync, as their Sterling last told ours in game.
    #[serde(default)]
    pub app: PartnerApp,
    #[serde(default)]
    pub look: Look,
}

/// Level, class and race for the app's window only. Never uploaded or written to the inbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Look {
    pub level: Option<i64>,
    /// The game's class token, e.g. "DRUID".
    pub class: Option<String>,
    pub race: Option<String>,
    /// The person's first name or nickname as set in Sterling ("Bran"), shown above the character name.
    pub nick: Option<String>,
    /// GetServerTime() of the last live sync with this partner in game.
    pub seen: Option<i64>,
}

const CLASSES: [&str; 13] = [
    "WARRIOR", "PALADIN", "HUNTER", "ROGUE", "PRIEST", "SHAMAN", "MAGE", "WARLOCK", "DRUID", "DEATHKNIGHT", "MONK", "DEMONHUNTER", "EVOKER",
];

fn look(t: &Table) -> Look {
    Look {
        level: int(t, "level").filter(|l| (1..=100).contains(l)),
        class: text(t, "class").filter(|c| CLASSES.contains(c)).map(String::from),
        race: text(t, "race").filter(|r| RE_NAME.is_match(r)).map(String::from),
        nick: text(t, "nick").filter(|r| RE_NAME.is_match(r)).map(String::from),
        seen: int(t, "seen").filter(|s| valid_time(*s)),
    }
}

/// `app = "0.1.9"` (has the app), `app = false` (seen in game, no app), or missing (not seen since the update).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum PartnerApp {
    #[default]
    Unknown,
    No,
    Has(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterOut {
    pub guid: String,
    pub name: String,
    pub realm: String,
    /// GetServerTime() at the save.
    pub saved: i64,
    pub proto: i64,
    /// Copy for your own other PCs (quest order, nicknames).
    pub self_packet: Option<String>,
    pub partners: Vec<PartnerOut>,
    /// Last server number Sterling applied per sender GUID (lets the app know what arrived).
    pub applied: BTreeMap<String, i64>,
    /// Auto-pairing: codes this character received in game from a Sterling partner, by sender GUID.
    #[serde(default)]
    pub codes: BTreeMap<String, String>,
    #[serde(default)]
    pub look: Look,
    /// Sterling's own version, for the window.
    #[serde(default)]
    pub sterling: Option<String>,
    /// Favourite partners as Sterling last saved them, with when they last changed.
    #[serde(default)]
    pub favs: Option<Favs>,
}

pub const MAX_FAVS: usize = 5;

/// Favourite partners (up to 5, in order) and when the list last changed. Whichever side changed it
/// last wins: Sterling through the outbox, the app through the inbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Favs {
    pub list: Vec<String>,
    pub change: i64,
}

impl Favs {
    /// Keeps only good GUIDs, no repeats, at most 5.
    pub fn clean(list: impl IntoIterator<Item = String>, change: i64) -> Favs {
        let mut out: Vec<String> = Vec::new();
        for g in list {
            if valid_guid(&g) && !out.contains(&g) && out.len() < MAX_FAVS {
                out.push(g);
            }
        }
        Favs { list: out, change }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outbox {
    pub characters: Vec<CharacterOut>,
    /// Entries dropped by validation, as short reasons (never their content).
    pub skipped: Vec<String>,
}

fn int(t: &Table, k: &str) -> Option<i64> {
    t.get(k).and_then(Value::as_i64)
}

fn text<'a>(t: &'a Table, k: &str) -> Option<&'a str> {
    t.get(k).and_then(Value::as_str)
}

fn valid_time(t: i64) -> bool {
    (1_000_000_000..4_000_000_000).contains(&t)
}

/// Reads the outbox file. Anything that doesn't validate is skipped, never guessed at.
pub fn parse_outbox(file_text: &str) -> Result<Outbox> {
    let vars = parse_saved_variables(file_text)?;
    let root = match vars.get("SterlingQuestSyncOut") {
        Some(Value::Table(t)) => t,
        _ => return Ok(Outbox::default()),
    };
    if int(root, "format") != Some(FORMAT) {
        return Err(Error::Format("outbox format is not 1".into()));
    }
    let mut out = Outbox::default();
    let chars = match root.get("characters").and_then(Value::as_table) {
        Some(c) => c,
        None => return Ok(out),
    };
    for (guid, v) in &chars.map {
        let Some(c) = v.as_table() else { continue };
        if !valid_guid(guid) {
            out.skipped.push("character with a bad GUID".into());
            continue;
        }
        let name = text(c, "name").unwrap_or("");
        let realm = text(c, "realm").unwrap_or("");
        let saved = int(c, "saved").unwrap_or(0);
        let proto = int(c, "proto").unwrap_or(0);
        if !RE_NAME.is_match(name) || !RE_REALM.is_match(realm) || !valid_time(saved) || !(1..=10_000).contains(&proto) {
            out.skipped.push(format!("{guid}: bad name, realm, time or protocol"));
            continue;
        }
        let self_packet = match text(c, "self") {
            Some(p) if valid_packet(p) => Some(p.to_string()),
            Some(_) => {
                out.skipped.push(format!("{guid}: bad self copy"));
                None
            }
            None => None,
        };
        let mut partners = Vec::new();
        if let Some(pt) = c.get("partners").and_then(Value::as_table) {
            for (pg, pv) in &pt.map {
                let Some(p) = pv.as_table() else { continue };
                if !valid_guid(pg) || pg == guid {
                    out.skipped.push(format!("{guid}: partner with a bad GUID"));
                    continue;
                }
                let removed = p.get("removed").and_then(Value::as_bool).unwrap_or(false);
                let change = int(p, "change").unwrap_or(0);
                if change < 0 {
                    out.skipped.push(format!("{guid}: partner with a bad change number"));
                    continue;
                }
                let packet = match text(p, "packet") {
                    Some(s) if valid_packet(s) => Some(s.to_string()),
                    Some(_) => {
                        out.skipped.push(format!("{guid}: bad partner copy"));
                        None
                    }
                    None => None,
                };
                let name = text(p, "name").filter(|n| RE_NAME.is_match(n)).map(String::from);
                let app = match p.get("app") {
                    Some(v) if v.as_bool() == Some(false) => PartnerApp::No,
                    Some(v) => match v.as_str() {
                        Some(ver) if valid_version(ver) => PartnerApp::Has(ver.to_string()),
                        _ => {
                            out.skipped.push(format!("{guid}: partner with a bad app version"));
                            PartnerApp::Unknown
                        }
                    },
                    None => PartnerApp::Unknown,
                };
                partners.push(PartnerOut { guid: pg.clone(), packet: if removed { None } else { packet }, change, removed, name, app, look: look(p) });
            }
        }
        let mut applied = BTreeMap::new();
        if let Some(at) = c.get("applied").and_then(Value::as_table) {
            for (k, v) in &at.map {
                if let Some(n) = v.as_i64() {
                    if valid_guid(k) && n >= 0 {
                        applied.insert(k.clone(), n);
                    }
                }
            }
        }
        let mut codes = BTreeMap::new();
        if let Some(ct) = c.get("codes").and_then(Value::as_table) {
            for (from, v) in &ct.map {
                let code = v.as_table().and_then(|t| text(t, "code")).or_else(|| v.as_str());
                if let Some(code) = code {
                    if valid_guid(from) && from != guid && RE_PAIR_CODE.is_match(code) {
                        codes.insert(from.clone(), code.to_string());
                    } else {
                        out.skipped.push(format!("{guid}: bad pairing code entry"));
                    }
                }
            }
        }
        let favs = match (c.get("favs").and_then(Value::as_table), int(c, "favs_change")) {
            (Some(t), Some(ch)) if valid_time(ch) => {
                Some(Favs::clean(t.list.iter().filter_map(|v| v.as_str().map(String::from)).filter(|g| g != guid), ch))
            }
            (Some(_), _) => {
                out.skipped.push(format!("{guid}: favourites without a good change time"));
                None
            }
            _ => None,
        };
        out.characters.push(CharacterOut {
            favs,
            look: look(c),
            sterling: text(c, "sterling").filter(|v| valid_version(v)).map(String::from),
            codes,
            guid: guid.clone(),
            name: name.to_string(),
            realm: realm.to_string(),
            saved,
            proto,
            self_packet,
            partners,
            applied,
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------ inbox

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    Partner,
    SelfCopy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InPacket {
    pub seq: i64,
    pub from: String,
    pub kind: Kind,
    pub time: i64,
    pub proto: i64,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PairStatus {
    Active,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InPair {
    pub partner: String,
    pub my_key: Option<String>,
    pub partner_key: Option<String>,
    pub status: PairStatus,
    pub change: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InChar {
    pub packets: Vec<InPacket>,
    pub pairs: Vec<InPair>,
    /// Auto-pairing: codes for Sterling to hand to these partners in game.
    #[serde(default)]
    pub offers: Vec<InOffer>,
    /// Favourite partners set in the app, for Sterling to take when newer than its own.
    #[serde(default)]
    pub favs: Option<Favs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InOffer {
    pub partner: String,
    pub code: String,
    pub expires: i64,
}

impl Inbox {
    pub fn set_offers(&mut self, to: &str, offers: Vec<InOffer>) -> bool {
        let c = self.chars.entry(to.to_string()).or_default();
        if c.offers == offers {
            return false;
        }
        c.offers = offers;
        true
    }

    pub fn set_favs(&mut self, to: &str, favs: Option<Favs>) -> bool {
        let c = self.chars.entry(to.to_string()).or_default();
        if c.favs == favs {
            return false;
        }
        c.favs = favs;
        true
    }
}

/// What the app keeps for the inbox, by target character GUID.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Inbox {
    pub chars: BTreeMap<String, InChar>,
    /// Versions for the add-on to show ("app update available" after a /reload).
    #[serde(default)]
    pub app: Option<AppInfo>,
}

/// Version numbers only: digits and dots.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AppInfo {
    /// This app's version.
    pub version: String,
    /// Newest app release, if known.
    pub latest: Option<String>,
    /// Newest Sterling release, if known.
    pub sterling_latest: Option<String>,
}

static RE_VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,4}\.\d{1,4}(\.\d{1,4})?$").unwrap());

pub fn valid_version(v: &str) -> bool {
    RE_VERSION.is_match(v)
}

impl Inbox {
    /// Adds a packet, replacing an older one from the same sender and kind. Never removes otherwise.
    pub fn put_packet(&mut self, to: &str, p: InPacket) -> bool {
        let c = self.chars.entry(to.to_string()).or_default();
        if let Some(old) = c.packets.iter_mut().find(|o| o.from == p.from && o.kind == p.kind) {
            if old.seq >= p.seq {
                return false;
            }
            *old = p;
        } else {
            c.packets.push(p);
        }
        c.packets.sort_by_key(|x| x.seq);
        true
    }

    pub fn set_pairs(&mut self, to: &str, pairs: Vec<InPair>) -> bool {
        let c = self.chars.entry(to.to_string()).or_default();
        if c.pairs == pairs {
            return false;
        }
        c.pairs = pairs;
        true
    }
}

fn check_inbox(inbox: &Inbox) -> Result<()> {
    let bad = |m: &str| Err(Error::Refused(format!("inbox value failed the allowlist: {m}")));
    if let Some(a) = &inbox.app {
        let ok = valid_version(&a.version) && [&a.latest, &a.sterling_latest].into_iter().flatten().all(|v| valid_version(v));
        if !ok {
            return bad("version");
        }
    }
    for (to, c) in &inbox.chars {
        if !valid_guid(to) {
            return bad("target GUID");
        }
        for p in &c.packets {
            if !valid_guid(&p.from) || p.seq < 0 || !valid_time(p.time) || !(1..=10_000).contains(&p.proto) || !valid_packet(&p.data) {
                return bad("packet");
            }
        }
        for o in &c.offers {
            if !valid_guid(&o.partner) || !RE_PAIR_CODE.is_match(&o.code) || !valid_time(o.expires) {
                return bad("offer");
            }
        }
        if let Some(f) = &c.favs {
            if f.list.len() > MAX_FAVS || !f.list.iter().all(|g| valid_guid(g)) || !valid_time(f.change) {
                return bad("favourites");
            }
        }
        for pr in &c.pairs {
            if !valid_guid(&pr.partner) || pr.change < 0 {
                return bad("pair");
            }
            for k in [&pr.my_key, &pr.partner_key].into_iter().flatten() {
                if !RE_PAIR_KEY.is_match(k) {
                    return bad("pair key");
                }
            }
        }
    }
    Ok(())
}

/// Builds Inbox.lua from the fixed template. Every value has already passed the allowlist,
/// and only digits, GUIDs, keys and packet characters ever reach the file.
pub fn render_inbox(inbox: &Inbox, written: i64) -> Result<String> {
    check_inbox(inbox)?;
    let mut s = String::new();
    s.push_str("-- Written by the Sterling Quest Sync app. Do not edit; it is replaced on every sync.\n");
    s.push_str("SterlingQuestSync_Inbox = {\n");
    s.push_str(&format!("  format = {},\n  written = {},\n", FORMAT, written));
    if let Some(a) = &inbox.app {
        let v = |x: &Option<String>| x.as_ref().map(|s| format!("\"{s}\"")).unwrap_or_else(|| "nil".into());
        s.push_str(&format!(
            "  app = {{ version = \"{}\", latest = {}, sterling_latest = {} }},\n",
            a.version,
            v(&a.latest),
            v(&a.sterling_latest)
        ));
    }
    s.push_str("  chars = {\n");
    for (to, c) in &inbox.chars {
        s.push_str(&format!("    [\"{to}\"] = {{\n      packets = {{\n"));
        for p in &c.packets {
            let kind = match p.kind {
                Kind::Partner => "partner",
                Kind::SelfCopy => "self",
            };
            s.push_str(&format!(
                "        {{ seq = {}, from = \"{}\", kind = \"{}\", time = {}, proto = {}, data = \"{}\" }},\n",
                p.seq, p.from, kind, p.time, p.proto, p.data
            ));
        }
        s.push_str("      },\n      pairs = {\n");
        for pr in &c.pairs {
            let status = match pr.status {
                PairStatus::Active => "active",
                PairStatus::Ended => "ended",
            };
            let key = |k: &Option<String>| match k {
                Some(v) => format!("\"{v}\""),
                None => "nil".to_string(),
            };
            s.push_str(&format!(
                "        {{ partner = \"{}\", my_key = {}, partner_key = {}, status = \"{}\", change = {} }},\n",
                pr.partner,
                key(&pr.my_key),
                key(&pr.partner_key),
                status,
                pr.change
            ));
        }
        s.push_str("      },\n      offers = {\n");
        for o in &c.offers {
            s.push_str(&format!("        {{ partner = \"{}\", code = \"{}\", expires = {} }},\n", o.partner, o.code, o.expires));
        }
        s.push_str("      },\n");
        if let Some(f) = &c.favs {
            s.push_str("      favs = {");
            for g in &f.list {
                s.push_str(&format!(" \"{g}\","));
            }
            s.push_str(&format!(" }},\n      favs_change = {},\n", f.change));
        }
        s.push_str("    },\n");
    }
    s.push_str("  },\n}\n");
    Ok(s)
}

/// The companion add-on's TOC, installed by the app.
pub fn companion_toc() -> String {
    [
        "## Interface: 16001, 11509",
        "## Title: Sterling Quest Sync",
        "## Notes: Data from the Sterling Quest Sync app. Leave enabled.",
        "## Author: SterlingAddons",
        "## IconTexture: Interface\\AddOns\\SterlingQuestSync\\icon",
        "## Version: 1",
        "## SavedVariables: SterlingQuestSyncOut",
        "",
        "Inbox.lua",
        "Core.lua",
        "",
    ]
    .join("\n")
}

pub fn empty_inbox_lua() -> String {
    render_inbox(&Inbox::default(), 1_000_000_000).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::parse_saved_variables;

    const ME: &str = "Player-6800-0000A11C";
    const BRAN: &str = "Player-6800-0A1B2C3D";

    fn sample_outbox() -> String {
        format!(
            r#"SterlingQuestSyncOut = {{
	["format"] = 1,
	["characters"] = {{
		["{ME}"] = {{
			["name"] = "Kaelith Emberfall",
			["realm"] = "CAMWOW US2 Realm 2 PvP",
			["saved"] = 1794770000,
			["proto"] = 6,
			["self"] = "SELFabc123",
			["partners"] = {{
				["{BRAN}"] = {{ ["packet"] = "STO1abc(def)", ["change"] = 3 }},
				["Player-6800-0000BEEF"] = {{ ["removed"] = true, ["change"] = 5 }},
				["Player-6800-00000BAD"] = {{ ["packet"] = "x\"]] os.exit() --", ["change"] = 1 }},
				["not-a-guid"] = {{ ["packet"] = "AAA", ["change"] = 1 }},
			}},
			["applied"] = {{ ["{BRAN}"] = 48213 }},
		}},
		["evil"] = {{ ["name"] = "X" }},
	}},
}}"#
        )
    }

    #[test]
    fn reads_and_validates_the_outbox() {
        let o = parse_outbox(&sample_outbox()).unwrap();
        assert_eq!(o.characters.len(), 1);
        let c = &o.characters[0];
        assert_eq!(c.name, "Kaelith Emberfall");
        assert_eq!(c.self_packet.as_deref(), Some("SELFabc123"));
        let bran = c.partners.iter().find(|p| p.guid == BRAN).unwrap();
        assert_eq!(bran.packet.as_deref(), Some("STO1abc(def)"));
        let removed = c.partners.iter().find(|p| p.guid == "Player-6800-0000BEEF").unwrap();
        assert!(removed.removed && removed.packet.is_none());
        let bad = c.partners.iter().find(|p| p.guid == "Player-6800-00000BAD").unwrap();
        assert!(bad.packet.is_none());
        assert!(!c.partners.iter().any(|p| p.guid == "not-a-guid"));
        assert_eq!(c.applied.get(BRAN), Some(&48213));
        assert!(o.skipped.len() >= 3);
    }

    #[test]
    fn reads_whether_each_partner_has_the_app() {
        let row = |g: &str, app: &str| format!("[\"{g}\"] = {{ [\"packet\"] = \"AB\", [\"change\"] = 1{app} }},");
        let text = format!(
            "SterlingQuestSyncOut = {{ [\"format\"] = 1, [\"characters\"] = {{ [\"{ME}\"] = {{ [\"name\"] = \"Kaelith Emberfall\", [\"realm\"] = \"R\", [\"saved\"] = 1794770000, [\"proto\"] = 6, [\"partners\"] = {{ {} {} {} {} }} }} }} }}",
            row("Player-6800-0000000A", ", [\"app\"] = \"0.1.9\""),
            row("Player-6800-0000000B", ", [\"app\"] = false"),
            row("Player-6800-0000000C", ""),
            row("Player-6800-0000000D", ", [\"app\"] = \"1.0; os.exit()\""),
        );
        let o = parse_outbox(&text).unwrap();
        let app = |g: &str| o.characters[0].partners.iter().find(|p| p.guid == g).unwrap().app.clone();
        assert_eq!(app("Player-6800-0000000A"), PartnerApp::Has("0.1.9".into()));
        assert_eq!(app("Player-6800-0000000B"), PartnerApp::No);
        assert_eq!(app("Player-6800-0000000C"), PartnerApp::Unknown);
        assert_eq!(app("Player-6800-0000000D"), PartnerApp::Unknown, "bad version ignored");
        assert!(o.skipped.iter().any(|s| s.contains("bad app version")));
    }

    #[test]
    fn reads_level_class_and_race_for_the_window() {
        let text = format!(
            "SterlingQuestSyncOut = {{ [\"format\"] = 1, [\"characters\"] = {{ [\"{ME}\"] = {{ [\"name\"] = \"Kaelith Emberfall\", [\"realm\"] = \"R\", [\"saved\"] = 1794770000, [\"proto\"] = 6, [\"level\"] = 41, [\"class\"] = \"DRUID\", [\"race\"] = \"Troll\", [\"sterling\"] = \"0.2.40\", [\"partners\"] = {{ [\"{BRAN}\"] = {{ [\"packet\"] = \"AB\", [\"change\"] = 1, [\"level\"] = 900, [\"class\"] = \"<b>MAGE\", [\"race\"] = \"Night Elf\", [\"nick\"] = \"Bran\", [\"seen\"] = 1794770100 }} }} }} }} }}"
        );
        let o = parse_outbox(&text).unwrap();
        let c = &o.characters[0];
        assert_eq!(c.look, Look { level: Some(41), class: Some("DRUID".into()), race: Some("Troll".into()), nick: None, seen: None });
        assert_eq!(c.sterling.as_deref(), Some("0.2.40"));
        let p = &c.partners[0].look;
        assert_eq!(p.level, None, "out of range");
        assert_eq!(p.class, None, "not a class token");
        assert_eq!(p.race.as_deref(), Some("Night Elf"));
        assert_eq!(p.nick.as_deref(), Some("Bran"));
        assert_eq!(p.seen, Some(1794770100));
    }

    #[test]
    fn reads_favourites_up_to_five() {
        let gs: Vec<String> = (0..7).map(|i| format!("\"Player-6800-0000000{i}\"")).collect();
        let text = format!(
            "SterlingQuestSyncOut = {{ [\"format\"] = 1, [\"characters\"] = {{ [\"{ME}\"] = {{ [\"name\"] = \"Kaelith Emberfall\", [\"realm\"] = \"R\", [\"saved\"] = 1794770000, [\"proto\"] = 6, [\"favs\"] = {{ \"bad\", \"{ME}\", \"Player-6800-00000000\", {} }}, [\"favs_change\"] = 1794770000 }} }} }}",
            gs.join(", ")
        );
        let f = parse_outbox(&text).unwrap().characters[0].favs.clone().unwrap();
        assert_eq!(f.list.len(), 5, "bad GUID, self and repeats dropped, capped at 5");
        assert_eq!(f.list[0], "Player-6800-00000000");
        assert_eq!(f.change, 1794770000);
    }

    #[test]
    fn missing_outbox_table_is_empty_not_an_error() {
        assert!(parse_outbox("SomethingElse = {}").unwrap().characters.is_empty());
        assert!(parse_outbox("SterlingQuestSyncOut = { [\"format\"] = 2 }").is_err());
    }

    fn sample_inbox() -> Inbox {
        let mut ib = Inbox::default();
        ib.put_packet(ME, InPacket { seq: 10, from: BRAN.into(), kind: Kind::Partner, time: 1794770000, proto: 6, data: "AAAA".into() });
        ib.set_pairs(ME, vec![InPair { partner: BRAN.into(), my_key: Some("ABCDEFGHJKMNPQRS".into()), partner_key: Some("TVWXYZ2345678923".into()), status: PairStatus::Active, change: 2 }]);
        ib
    }

    #[test]
    fn inbox_renders_to_lua_our_own_parser_reads_back() {
        let lua = render_inbox(&sample_inbox(), 1794770123).unwrap();
        let v = parse_saved_variables(&lua).unwrap();
        let root = v["SterlingQuestSync_Inbox"].as_table().unwrap();
        assert_eq!(root.get("format").unwrap().as_i64(), Some(1));
        let me = root.get("chars").unwrap().as_table().unwrap().get(ME).unwrap().as_table().unwrap();
        let p = &me.get("packets").unwrap().as_table().unwrap().list[0];
        assert_eq!(p.as_table().unwrap().get("data").unwrap().as_str(), Some("AAAA"));
        assert!(!lua.contains("Kaelith Emberfall"), "no names in the inbox");
    }

    #[test]
    fn inbox_refuses_anything_off_the_allowlist() {
        for data in ["x\"]] os.exit() --", "abc\ndef", "a b", "a'b", "a\\b", ""] {
            let mut ib = Inbox::default();
            ib.chars.entry(ME.into()).or_default().packets.push(InPacket { seq: 1, from: BRAN.into(), kind: Kind::Partner, time: 1794770000, proto: 6, data: data.into() });
            assert!(render_inbox(&ib, 1794770000).is_err(), "{data:?}");
        }
        let mut ib = Inbox::default();
        ib.chars.entry("Player-1-0\"]] x".into()).or_default();
        assert!(render_inbox(&ib, 1794770000).is_err());
        let mut ib = sample_inbox();
        ib.chars.get_mut(ME).unwrap().pairs[0].my_key = Some("abc\"..os.exit()".into());
        assert!(render_inbox(&ib, 1794770000).is_err());
        let mut ib = sample_inbox();
        ib.chars.get_mut(ME).unwrap().packets[0].from = "Player-6800-0A1B2C3D\"".into();
        assert!(render_inbox(&ib, 1794770000).is_err());
    }

    #[test]
    fn newer_packets_replace_older_and_never_go_backwards() {
        let mut ib = sample_inbox();
        let older = InPacket { seq: 5, from: BRAN.into(), kind: Kind::Partner, time: 1794770000, proto: 6, data: "OLD".into() };
        assert!(!ib.put_packet(ME, older));
        let newer = InPacket { seq: 20, from: BRAN.into(), kind: Kind::Partner, time: 1794770100, proto: 6, data: "NEW".into() };
        assert!(ib.put_packet(ME, newer));
        assert_eq!(ib.chars[ME].packets.len(), 1);
        assert_eq!(ib.chars[ME].packets[0].data, "NEW");
    }

    #[test]
    fn app_versions_go_in_and_bad_ones_are_refused() {
        let mut ib = sample_inbox();
        ib.app = Some(AppInfo { version: "0.1.2".into(), latest: Some("0.1.3".into()), sterling_latest: None });
        let lua = render_inbox(&ib, 1794770123).unwrap();
        let v = parse_saved_variables(&lua).unwrap();
        let app = v["SterlingQuestSync_Inbox"].as_table().unwrap().get("app").unwrap().as_table().unwrap();
        assert_eq!(app.get("version").unwrap().as_str(), Some("0.1.2"));
        assert_eq!(app.get("latest").unwrap().as_str(), Some("0.1.3"));
        for bad in ["0.1.3\" os.exit() --", "v0.1", "", "1.2.3.4"] {
            ib.app = Some(AppInfo { version: "0.1.2".into(), latest: Some(bad.into()), sterling_latest: None });
            assert!(render_inbox(&ib, 1794770123).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn empty_inbox_is_valid_lua() {
        let lua = empty_inbox_lua();
        assert!(parse_saved_variables(&lua).unwrap().contains_key("SterlingQuestSync_Inbox"));
    }
}
