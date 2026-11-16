//! End to end: three fake PCs (Kael, Kael's second PC, Bran) against a real server.
//!
//! Skipped unless SQS_E2E_SERVER is set, for example to a local `wrangler dev` of the server repo:
//!   SQS_E2E_SERVER=http://127.0.0.1:8787 cargo test --test e2e -- --nocapture
//! It deletes the accounts it made at the end.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sqs_core::api::{new_key, Client};
use sqs_core::engine::{load_state, sync, State};
use sqs_core::formats::{companion_toc, parse_outbox, Kind, PairStatus};
use sqs_core::lua::parse_saved_variables;
use sqs_core::paths;
use sqs_core::safety::{Root, SafeFs};
use sqs_core::{companion, Error};

const KAEL: &str = "Player-6800-0000A11C";
const BRAN: &str = "Player-6800-0A1B2C3D";
const REALM: &str = "CAMWOW US2 Realm 2 PvP";

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

struct Pc {
    _dir: tempfile::TempDir,
    fs: SafeFs,
    api: Client,
    state: State,
    acct: String,
}

impl Pc {
    fn new(server: &str, wtf_account: &str) -> Pc {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("World of Warcraft/_forever_");
        fs::create_dir_all(game.join("Interface/AddOns/Sterling")).unwrap();
        fs::create_dir_all(paths::saved_variables(&game, wtf_account)).unwrap();
        fs::write(paths::sterling_saved(&game, wtf_account), "SterlingDB = {}").unwrap();
        let fs_ = SafeFs::new(&game, &dir.path().join("appdata")).unwrap();
        companion::install(&fs_, &[("SterlingQuestSync.toc", companion_toc().as_bytes())]).unwrap();
        let state = load_state(&fs_);
        Pc { api: Client::new(server, &new_key()).unwrap(), fs: fs_, state, _dir: dir, acct: wtf_account.into() }
    }

    /// Stands in for the game writing the companion's SavedVariables at logout.
    fn game_saves(&self, guid: &str, name: &str, saved: i64, self_copy: &str, partners: &[(&str, Option<&str>, i64)]) {
        self.game_saves_codes(guid, name, saved, self_copy, partners, &[]);
    }

    /// Same, plus codes Sterling received from partners in game (auto-pairing).
    fn game_saves_codes(&self, guid: &str, name: &str, saved: i64, self_copy: &str, partners: &[(&str, Option<&str>, i64)], codes: &[(&str, &str)]) {
        let mut c = String::new();
        for (from, code) in codes {
            c.push_str(&format!("[\"{from}\"] = {{ [\"code\"] = \"{code}\", [\"time\"] = {saved} }},\n"));
        }
        let mut p = String::new();
        for (g, packet, change) in partners {
            match packet {
                Some(pk) => p.push_str(&format!("[\"{g}\"] = {{ [\"packet\"] = \"{pk}\", [\"change\"] = {change}, [\"app\"] = \"0.1.9\" }},\n")),
                None => p.push_str(&format!("[\"{g}\"] = {{ [\"removed\"] = true, [\"change\"] = {change} }},\n")),
            }
        }
        let text = format!(
            "SterlingQuestSyncOut = {{\n[\"format\"] = 1,\n[\"characters\"] = {{\n[\"{guid}\"] = {{\n[\"name\"] = \"{name}\",\n[\"realm\"] = \"{REALM}\",\n[\"saved\"] = {saved},\n[\"proto\"] = 6,\n[\"self\"] = \"{self_copy}\",\n[\"partners\"] = {{\n{p}}},\n[\"codes\"] = {{\n{c}}},\n}},\n}},\n}}\n"
        );
        fs::write(paths::outbox_file(self.fs.game(), &self.acct), text).unwrap();
    }

    fn sync(&mut self) -> sqs_core::engine::Report {
        let o = match fs::read_to_string(paths::outbox_file(self.fs.game(), &self.acct)) {
            Ok(t) => vec![parse_outbox(&t).unwrap()],
            Err(_) => vec![],
        };
        sync(&self.api, &self.fs, &mut self.state, &o, None, now()).unwrap()
    }

    fn inbox_lua(&self) -> String {
        fs::read_to_string(self.fs.root(Root::Companion).join("Inbox.lua")).unwrap()
    }

    fn wtf_snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut v = Vec::new();
        walk(&self.fs.game().join("WTF"), &mut v);
        v.sort();
        v
    }
}

fn walk(d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    for e in fs::read_dir(d).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push((p.clone(), fs::read(&p).unwrap()));
        }
    }
}

#[test]
fn three_pcs_full_flow() {
    let Ok(server) = std::env::var("SQS_E2E_SERVER") else {
        eprintln!("SQS_E2E_SERVER not set, skipping");
        return;
    };
    let t0 = now();

    let mut kael = Pc::new(&server, "TESTACCOUNT");
    let mut bran = Pc::new(&server, "PARTNERACCT");
    let ra = kael.api.join("Kael PC", None).unwrap();
    let sa = bran.api.join("Bran PC", None).unwrap();
    assert!(ra.recovery_code.as_deref().unwrap().starts_with("SQS-"));
    assert_ne!(ra.account_id, sa.account_id);

    // Both log out once: characters get listed, self copies go up.
    kael.game_saves(KAEL, "Kaelith Emberfall", t0, "KAELSELF1", &[]);
    bran.game_saves(BRAN, "Brannoc Ashvale", t0, "BRANSELF1", &[]);
    assert_eq!(kael.sync().uploaded, 1);
    assert_eq!(bran.sync().uploaded, 1);

    // Kael makes a code, Bran checks it and accepts.
    let code = kael.api.pair_code(KAEL).unwrap();
    let peek = bran.api.peek_code(BRAN, &code.code).unwrap();
    assert_eq!(peek.partner.name, "Kaelith Emberfall");
    assert!(!peek.already_paired && !peek.different_install);
    bran.api.redeem_code(BRAN, &code.code, false).unwrap();
    assert!(bran.api.redeem_code(BRAN, &code.code, false).is_err(), "single use");

    // Next logout carries a copy for the partner.
    kael.game_saves(KAEL, "Kaelith Emberfall", t0 + 60, "KAELSELF2", &[(BRAN, Some("FROMKAEL1"), 1)]);
    let r = kael.sync();
    assert_eq!(r.uploaded, 2, "{r:?}");

    let before = bran.wtf_snapshot();
    let r = bran.sync();
    assert!(r.inbox_written && r.received >= 1, "{r:?}");
    assert_eq!(before, bran.wtf_snapshot(), "WTF untouched by a sync");
    let lua = bran.inbox_lua();
    let v = parse_saved_variables(&lua).unwrap();
    let me = v["SterlingQuestSync_Inbox"].as_table().unwrap().get("chars").unwrap().as_table().unwrap().get(BRAN).unwrap().as_table().unwrap();
    let pk = me.get("packets").unwrap().as_table().unwrap().list.iter().map(|p| p.as_table().unwrap().get("data").unwrap().as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(pk.contains(&"FROMKAEL1".to_string()), "{pk:?}");
    assert!(!lua.contains("Kaelith Emberfall"), "no names in the inbox");
    let pair = &bran.state.inbox.chars[BRAN].pairs[0];
    assert_eq!(pair.status, PairStatus::Active);
    assert!(pair.my_key.is_some() && pair.partner_key.is_some());
    let kael_pair_keys = {
        kael.sync();
        let p = &kael.state.inbox.chars[KAEL].pairs[0];
        (p.my_key.clone(), p.partner_key.clone())
    };
    assert_eq!(kael_pair_keys, (pair.partner_key.clone(), pair.my_key.clone()), "keys mirror on both sides");

    // Bran answers; Kael receives.
    bran.game_saves(BRAN, "Brannoc Ashvale", t0 + 120, "BRANSELF2", &[(KAEL, Some("FROMBRAN1"), 1)]);
    bran.sync();
    kael.sync();
    assert!(kael.state.inbox.chars[KAEL].packets.iter().any(|p| p.data == "FROMBRAN1" && p.kind == Kind::Partner));

    // An older save from another install can't overwrite a newer one.
    kael.game_saves(KAEL, "Kaelith Emberfall", t0 + 30, "KAELOLD", &[(BRAN, Some("OLDCOPY"), 1)]);
    kael.state.sent.clear();
    let r = kael.sync();
    assert_eq!(r.stale.len(), 2, "{r:?}");
    bran.sync();
    assert!(!bran.inbox_lua().contains("OLDCOPY"));

    // A second PC joins Kael's account with a link code and gets the self copy.
    let link = kael.api.link_code().unwrap();
    let mut kael2 = Pc::new(&server, "TESTACCOUNT");
    let j = kael2.api.redeem_link(&link.code, "Kael laptop").unwrap();
    assert_eq!(j.account_id, ra.account_id);
    assert!(kael2.api.redeem_link(&link.code, "again").is_err(), "link code single use");
    assert_eq!(kael2.api.account().unwrap().pcs.len(), 2);
    kael2.state.listed.insert(KAEL.into(), sqs_core::api::Character { guid: KAEL.into(), name: "Kaelith Emberfall".into(), realm: REALM.into() });
    kael2.sync();
    let c = &kael2.state.inbox.chars[KAEL];
    assert!(c.packets.iter().any(|p| p.kind == Kind::SelfCopy && p.data == "KAELSELF2"), "{:?}", c.packets);
    assert!(c.packets.iter().any(|p| p.data == "FROMBRAN1"), "partners carry over to the new PC");
    assert!(c.pairs.iter().any(|p| p.status == PairStatus::Active));

    // Bran removes Kael in game: the pairing ends and Kael's side drops her copy and keys.
    bran.game_saves(BRAN, "Brannoc Ashvale", t0 + 180, "BRANSELF3", &[(KAEL, None, now() + 5)]);
    let r = bran.sync();
    assert_eq!(r.ended.len(), 1, "{r:?}");
    kael.sync();
    let c = &kael.state.inbox.chars[KAEL];
    assert_eq!(c.pairs[0].status, PairStatus::Ended);
    assert!(c.pairs[0].my_key.is_none());
    assert!(!c.packets.iter().any(|p| p.kind == Kind::Partner));

    // Delete my data: both accounts gone, keys stop working.
    kael.api.delete_account().unwrap();
    bran.api.delete_account().unwrap();
    match kael2.api.account() {
        Err(Error::Api { status: 401, code, .. }) => assert_eq!(code, "unknown_key"),
        other => panic!("expected unknown_key, got {other:?}"),
    }
    eprintln!("e2e ok");
}

#[test]
fn sterling_partners_pair_on_their_own() {
    let Ok(server) = std::env::var("SQS_E2E_SERVER") else {
        eprintln!("SQS_E2E_SERVER not set, skipping");
        return;
    };
    let t0 = now();
    let mut kael = Pc::new(&server, "TESTACCOUNT");
    let mut bran = Pc::new(&server, "PARTNERACCT");
    kael.api.join("Kael PC", None).unwrap();
    bran.api.join("Bran PC", None).unwrap();

    // Already Sterling partners in game, never paired in the app.
    kael.game_saves(KAEL, "Kaelith Emberfall", t0, "RS1", &[(BRAN, Some("R2S1"), 1)]);
    bran.game_saves(BRAN, "Brannoc Ashvale", t0, "SS1", &[(KAEL, Some("S2R1"), 1)]);
    let r = kael.sync();
    assert_eq!(r.auto_offered, 1, "{r:?}");
    assert_eq!(r.not_paired.len(), 1);
    let offer = kael.state.inbox.chars[KAEL].offers[0].clone();
    assert_eq!(offer.partner, BRAN);
    assert!(kael.inbox_lua().contains(&offer.code));
    bran.sync();

    // Kael's Sterling hands the code to Bran in game; her Sterling saves it at logout.
    bran.game_saves_codes(BRAN, "Brannoc Ashvale", t0 + 60, "SS2", &[(KAEL, Some("S2R2"), 1)], &[(KAEL, &offer.code)]);
    let r = bran.sync();
    assert_eq!(r.auto_paired.len(), 1, "{r:?}");
    assert_eq!(r.auto_paired[0].2, "Kaelith Emberfall");
    assert!(r.sent_to.contains(&(BRAN.to_string(), KAEL.to_string())), "her copy goes up right away");
    assert!(bran.state.inbox.chars[BRAN].offers.is_empty(), "her own offer is cancelled");
    assert!(bran.state.inbox.chars[BRAN].pairs.iter().any(|p| p.partner == KAEL && p.status == PairStatus::Active));

    // Kael's next sync sees the pairing, drops his offer and gets her copy.
    let r = kael.sync();
    assert!(kael.state.inbox.chars[KAEL].offers.is_empty(), "{r:?}");
    assert!(kael.state.inbox.chars[KAEL].packets.iter().any(|p| p.data == "S2R2"));
    assert!(kael.api.open_codes().unwrap().is_empty(), "no codes left open");
    assert!(bran.api.open_codes().unwrap().is_empty());

    // The same code again does nothing.
    let r = bran.sync();
    assert!(r.auto_paired.is_empty());

    kael.api.delete_account().unwrap();
    bran.api.delete_account().unwrap();
    eprintln!("auto-pairing e2e ok");
}
