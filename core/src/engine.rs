//! One sync pass: outbox up, inbox down. Per-PC state lives in the app's data folder.
//!
//! Order matters for safety: everything is worked out on a copy of the state, Inbox.lua is written
//! (after a backup of Sterling's SavedVariables), and only then is the new state saved. A crash
//! part way leaves the old state, so the next pass simply fetches the same packets again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::{Api, Character, Pairing, Upload};
use crate::companion;
use crate::formats::{valid_guid, valid_packet, InPacket, InPair, Inbox, Kind, Outbox, PairStatus, RE_PAIR_KEY};
use crate::safety::{Root, SafeFs};
use crate::{Error, Result};

pub const STATE_FORMAT: u32 = 1;
const MAX_INBOX_PAGES: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sent {
    pub game_time: i64,
    pub hash: String,
    pub seq: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub format: u32,
    /// Highest server number already in the inbox.
    pub since: i64,
    pub inbox: Inbox,
    /// Characters as listed on the server, by GUID.
    pub listed: BTreeMap<String, Character>,
    /// Last upload per slot, keyed "from>to".
    pub sent: BTreeMap<String, Sent>,
    pub last_sync: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report {
    pub uploaded: usize,
    pub unchanged: usize,
    /// Slots where the server already had a newer save (from, to).
    pub stale: Vec<(String, String)>,
    /// Partners in the game with no live pairing yet (from, to).
    pub not_paired: Vec<(String, String)>,
    pub received: usize,
    pub ended: Vec<String>,
    pub inbox_written: bool,
    pub skipped: Vec<String>,
}

fn slot_key(from: &str, to: &str) -> String {
    format!("{from}>{to}")
}

fn hash(packet: &str) -> String {
    let d = Sha256::digest(packet.as_bytes());
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

fn state_path(fs_: &SafeFs) -> std::path::PathBuf {
    fs_.root(Root::AppData).join("state.json")
}

pub fn load_state(fs_: &SafeFs) -> State {
    fs::read_to_string(state_path(fs_))
        .ok()
        .and_then(|t| serde_json::from_str::<State>(&t).ok())
        .filter(|s| s.format == STATE_FORMAT)
        .unwrap_or(State { format: STATE_FORMAT, ..Default::default() })
}

pub fn save_state(fs_: &SafeFs, s: &State) -> Result<()> {
    let text = serde_json::to_string_pretty(s).map_err(|e| Error::Other(e.to_string()))?;
    fs_.write_atomic(&state_path(fs_), text.as_bytes())
}

/// Characters with a save newer than what this PC last uploaded: the "ready to sync" line.
pub fn pending(state: &State, outboxes: &[Outbox]) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for o in outboxes {
        for c in &o.characters {
            let mut slots: Vec<(&str, &str)> = Vec::new();
            if let Some(p) = &c.self_packet {
                slots.push((&c.guid, p));
            }
            // Partners count only when the last sync saw a live pairing with them.
            let live = |g: &str| {
                state.inbox.chars.get(&c.guid).is_some_and(|ch| ch.pairs.iter().any(|p| p.partner == g && p.status == PairStatus::Active))
            };
            for p in &c.partners {
                if let Some(pk) = &p.packet {
                    if live(&p.guid) {
                        slots.push((&p.guid, pk));
                    }
                }
            }
            let ready = slots.iter().any(|(to, pk)| match state.sent.get(&slot_key(&c.guid, to)) {
                Some(s) => s.game_time < c.saved && s.hash != hash(pk),
                None => true,
            });
            if ready {
                out.push((c.guid.clone(), c.saved));
            }
        }
    }
    out
}

fn active_pair<'a>(pairings: &'a [Pairing], me: &str, partner: &str) -> Option<&'a Pairing> {
    pairings.iter().find(|p| p.status == "active" && p.ended_at.is_none() && p.my_guid == me && p.partner_guid == partner)
}

fn pair_key(k: &Option<String>) -> Option<String> {
    k.as_ref().filter(|v| RE_PAIR_KEY.is_match(v)).cloned()
}

/// Turns the server's pairings into the inbox's pairs per character.
fn pairs_for(pairings: &[Pairing], me: &str) -> Vec<InPair> {
    // One entry per partner: an active pairing beats an ended one; newer beats older.
    let mut best: BTreeMap<&str, &Pairing> = BTreeMap::new();
    for p in pairings.iter().filter(|p| p.my_guid == me && valid_guid(&p.partner_guid)) {
        let rank = |x: &Pairing| (x.status == "active", x.ended_at.unwrap_or(x.created_at).max(x.created_at));
        match best.get(p.partner_guid.as_str()) {
            Some(cur) if rank(cur) >= rank(p) => {}
            _ => {
                best.insert(&p.partner_guid, p);
            }
        }
    }
    best.values()
        .map(|p| {
            let active = p.status == "active" && p.ended_at.is_none();
            InPair {
                partner: p.partner_guid.clone(),
                my_key: if active { pair_key(&p.my_key) } else { None },
                partner_key: if active { pair_key(&p.partner_key) } else { None },
                status: if active { PairStatus::Active } else { PairStatus::Ended },
                change: if active { p.created_at } else { p.ended_at.unwrap_or(p.created_at) },
            }
        })
        .collect()
}

/// One full pass. `now` is the wall clock, used only for `written` and `last_sync`.
pub fn sync(api: &dyn Api, fs_: &SafeFs, state: &mut State, outboxes: &[Outbox], now: i64) -> Result<Report> {
    let mut st = state.clone();
    st.format = STATE_FORMAT;
    let mut rep = Report::default();
    for o in outboxes {
        rep.skipped.extend(o.skipped.iter().cloned());
    }

    // 1. Make sure every character in the outbox is listed, with its current name.
    for c in outboxes.iter().flat_map(|o| &o.characters) {
        let ch = Character { guid: c.guid.clone(), name: c.name.clone(), realm: c.realm.clone() };
        if st.listed.get(&c.guid) != Some(&ch) {
            api.put_character(&ch)?;
            st.listed.insert(c.guid.clone(), ch);
        }
    }

    // 2. Partners removed in game end the pairing, but only one made before the removal.
    let mut pairings = api.pairings()?;
    let mut ended_any = false;
    for c in outboxes.iter().flat_map(|o| &o.characters) {
        for p in c.partners.iter().filter(|p| p.removed) {
            if let Some(pr) = active_pair(&pairings, &c.guid, &p.guid) {
                if p.change > pr.created_at {
                    api.end_pairing(&pr.id)?;
                    rep.ended.push(pr.id.clone());
                    ended_any = true;
                }
            }
        }
    }
    if ended_any {
        pairings = api.pairings()?;
    }

    // 3. Uploads: the self copy, then one per live partner.
    for c in outboxes.iter().flat_map(|o| &o.characters) {
        let mut slots: Vec<(&str, &str)> = Vec::new();
        if let Some(p) = &c.self_packet {
            slots.push((&c.guid, p));
        }
        for p in c.partners.iter().filter(|p| !p.removed) {
            let Some(pk) = &p.packet else { continue };
            if active_pair(&pairings, &c.guid, &p.guid).is_none() {
                rep.not_paired.push((c.guid.clone(), p.guid.clone()));
                continue;
            }
            slots.push((&p.guid, pk));
        }
        for (to, pk) in slots {
            let k = slot_key(&c.guid, to);
            let h = hash(pk);
            if let Some(s) = st.sent.get(&k) {
                if s.game_time >= c.saved && (s.hash == h || s.game_time > c.saved) {
                    rep.unchanged += 1;
                    continue;
                }
            }
            match api.put_slot(&c.guid, to, pk, c.saved, c.proto) {
                Ok(Upload::Saved(seq)) => {
                    rep.uploaded += 1;
                    st.sent.insert(k, Sent { game_time: c.saved, hash: h, seq });
                }
                Ok(Upload::Unchanged(seq)) => {
                    rep.unchanged += 1;
                    st.sent.insert(k, Sent { game_time: c.saved, hash: h, seq });
                }
                Ok(Upload::Stale { seq, game_time }) => {
                    rep.stale.push((c.guid.clone(), to.to_string()));
                    // Remember the newer save so this one isn't sent again.
                    st.sent.insert(k, Sent { game_time, hash: String::new(), seq });
                }
                Err(Error::Api { code, .. }) if code == "not_paired" => {
                    rep.not_paired.push((c.guid.clone(), to.to_string()));
                }
                Err(e) => return Err(e),
            }
        }
    }

    // 4. Downloads, page by page.
    let before = st.inbox.clone();
    let mine: BTreeSet<String> = st.listed.keys().cloned().collect();
    for _ in 0..MAX_INBOX_PAGES {
        let page = api.inbox(st.since)?;
        for p in &page.packets {
            let kind = match p.kind.as_str() {
                "partner" => Kind::Partner,
                "self" => Kind::SelfCopy,
                _ => {
                    rep.skipped.push("packet of an unknown kind".into());
                    continue;
                }
            };
            let fine = valid_guid(&p.from_guid)
                && valid_guid(&p.to_guid)
                && valid_packet(&p.packet)
                && (1_000_000_000..4_000_000_000).contains(&p.game_time)
                && (1..=10_000).contains(&p.proto)
                && p.seq > 0;
            if !fine || (kind == Kind::SelfCopy && p.from_guid != p.to_guid) {
                rep.skipped.push("packet that failed checks".into());
                continue;
            }
            // Our own upload coming back: nothing new for this PC.
            if kind == Kind::SelfCopy && st.sent.get(&slot_key(&p.from_guid, &p.to_guid)).map(|s| s.seq) == Some(p.seq) {
                continue;
            }
            let ip = InPacket { seq: p.seq, from: p.from_guid.clone(), kind, time: p.game_time, proto: p.proto, data: p.packet.clone() };
            if st.inbox.put_packet(&p.to_guid, ip) {
                rep.received += 1;
            }
        }
        st.since = st.since.max(page.next);
        if !page.more {
            break;
        }
    }

    // 5. Pairs for every character that has any, and drop copies from ended pairings.
    let mut targets: BTreeSet<String> = st.inbox.chars.keys().cloned().collect();
    targets.extend(pairings.iter().map(|p| p.my_guid.clone()).filter(|g| valid_guid(g)));
    for t in &targets {
        let pairs = pairs_for(&pairings, t);
        let live: BTreeSet<&str> = pairs.iter().filter(|p| p.status == PairStatus::Active).map(|p| p.partner.as_str()).collect();
        st.inbox.set_pairs(t, pairs.clone());
        if let Some(c) = st.inbox.chars.get_mut(t) {
            c.packets.retain(|p| p.kind == Kind::SelfCopy || live.contains(p.from.as_str()));
        }
    }
    st.inbox.chars.retain(|g, c| !c.packets.is_empty() || !c.pairs.is_empty() || mine.contains(g));

    // 6. Write the inbox (backup first), then commit the state.
    if st.inbox != before || !companion::is_installed(fs_) {
        if st.inbox != before {
            companion::backup_saved_variables(fs_, "sync")?;
        }
        rep.inbox_written = companion::write_inbox(fs_, &st.inbox, now)?;
    }
    st.last_sync = Some(now);
    save_state(fs_, &st)?;
    *state = st;
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{InboxPage, ServerPacket};
    use crate::formats::{parse_outbox, CharacterOut, PartnerOut};
    use std::cell::RefCell;

    const ME: &str = "Player-6800-0000A11C";
    const BRAN: &str = "Player-6800-0A1B2C3D";
    const ALT: &str = "Player-6800-0000ABCD";

    #[derive(Default)]
    struct Fake {
        pairings: RefCell<Vec<Pairing>>,
        slots: RefCell<BTreeMap<String, (i64, String, i64)>>,
        incoming: RefCell<Vec<ServerPacket>>,
        puts: RefCell<usize>,
        chars: RefCell<usize>,
        ended: RefCell<Vec<String>>,
        seq: RefCell<i64>,
    }

    impl Api for Fake {
        fn put_character(&self, _c: &Character) -> Result<()> {
            *self.chars.borrow_mut() += 1;
            Ok(())
        }
        fn pairings(&self) -> Result<Vec<Pairing>> {
            Ok(self.pairings.borrow().clone())
        }
        fn end_pairing(&self, id: &str) -> Result<()> {
            self.ended.borrow_mut().push(id.into());
            for p in self.pairings.borrow_mut().iter_mut().filter(|p| p.id == id) {
                p.ended_at = Some(1_794_779_000);
                p.status = "ended".into();
                p.my_key = None;
                p.partner_key = None;
            }
            Ok(())
        }
        fn put_slot(&self, from: &str, to: &str, packet: &str, gt: i64, _proto: i64) -> Result<Upload> {
            *self.puts.borrow_mut() += 1;
            let k = slot_key(from, to);
            let mut slots = self.slots.borrow_mut();
            if let Some((cur, _, seq)) = slots.get(&k) {
                if *cur > gt {
                    return Ok(Upload::Stale { seq: *seq, game_time: *cur });
                }
            }
            *self.seq.borrow_mut() += 1;
            let s = *self.seq.borrow();
            slots.insert(k, (gt, packet.into(), s));
            Ok(Upload::Saved(s))
        }
        fn inbox(&self, since: i64) -> Result<InboxPage> {
            let all: Vec<_> = self.incoming.borrow().iter().filter(|p| p.seq > since).cloned().collect();
            let more = all.len() > 2;
            let packets: Vec<_> = all.into_iter().take(2).collect();
            let next = packets.last().map(|p| p.seq).unwrap_or(since);
            Ok(InboxPage { packets, next, more })
        }
    }

    fn pairing(id: &str, me: &str, partner: &str, created: i64) -> Pairing {
        Pairing {
            id: id.into(),
            created_at: created,
            ended_at: None,
            ended_reason: None,
            my_guid: me.into(),
            partner_guid: partner.into(),
            partner_name: "Bran".into(),
            partner_realm: "CAMWOW US2 Realm 2 PvP".into(),
            my_key: Some("ABCDEFGHJKMNPQRS".into()),
            partner_key: Some("TVWXYZ2345678923".into()),
            status: "active".into(),
        }
    }

    fn outbox(saved: i64, packet: &str, removed: Option<i64>) -> Outbox {
        let mut partners = vec![PartnerOut { guid: BRAN.into(), packet: Some(packet.into()), change: 1, removed: false }];
        if let Some(ch) = removed {
            partners[0] = PartnerOut { guid: BRAN.into(), packet: None, change: ch, removed: true };
        }
        partners.push(PartnerOut { guid: ALT.into(), packet: Some("ALTCOPY".into()), change: 1, removed: false });
        Outbox {
            characters: vec![CharacterOut {
                guid: ME.into(),
                name: "Kaelith Emberfall".into(),
                realm: "CAMWOW US2 Realm 2 PvP".into(),
                saved,
                proto: 6,
                self_packet: Some("SELF1".into()),
                partners,
                applied: Default::default(),
            }],
            skipped: vec![],
        }
    }

    fn incoming(seq: i64, from: &str, to: &str, kind: &str, data: &str) -> ServerPacket {
        ServerPacket { from_guid: from.into(), to_guid: to.into(), packet: data.into(), game_time: 1_794_770_000 + seq, proto: 6, seq, kind: kind.into() }
    }

    fn setup() -> (tempfile::TempDir, SafeFs) {
        let t = tempfile::tempdir().unwrap();
        let g = t.path().join("_forever_");
        fs::create_dir_all(g.join("Interface/AddOns/Questie")).unwrap();
        let sv = g.join("WTF/Account/TESTACCOUNT/SavedVariables");
        fs::create_dir_all(&sv).unwrap();
        fs::write(sv.join("Sterling.lua"), "SterlingDB = {}").unwrap();
        let s = SafeFs::new(&g, &t.path().join("appdata")).unwrap();
        companion::install(&s, &[("SterlingQuestSync.toc", crate::formats::companion_toc().as_bytes())]).unwrap();
        (t, s)
    }

    #[test]
    fn uploads_once_then_skips_until_a_new_save() {
        let (_t, fs_) = setup();
        let api = Fake::default();
        api.pairings.borrow_mut().push(pairing("p1", ME, BRAN, 1_794_000_000));
        let mut st = load_state(&fs_);
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "COPY1", None)], 1_794_770_100).unwrap();
        assert_eq!(r.uploaded, 2, "self and Bran");
        assert_eq!(r.not_paired, vec![(ME.to_string(), ALT.to_string())]);
        assert_eq!(*api.chars.borrow(), 1);
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "COPY1", None)], 1_794_770_200).unwrap();
        assert_eq!(r.uploaded, 0);
        assert_eq!(*api.chars.borrow(), 1, "already listed");
        assert!(pending(&st, &[outbox(1_794_770_000, "COPY1", None)]).is_empty());
        assert_eq!(pending(&st, &[outbox(1_794_770_500, "COPY2", None)]).len(), 1);
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_500, "COPY2", None)], 1_794_770_600).unwrap();
        assert_eq!(r.uploaded, 2);
    }

    #[test]
    fn an_older_save_never_replaces_a_newer_one() {
        let (_t, fs_) = setup();
        let api = Fake::default();
        api.pairings.borrow_mut().push(pairing("p1", ME, BRAN, 1_794_000_000));
        api.slots.borrow_mut().insert(slot_key(ME, BRAN), (1_794_779_999, "NEWER".into(), 99));
        let mut st = load_state(&fs_);
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "OLDER", None)], 1).unwrap();
        assert_eq!(r.stale, vec![(ME.to_string(), BRAN.to_string())]);
        assert_eq!(api.slots.borrow()[&slot_key(ME, BRAN)].1, "NEWER");
        let puts = *api.puts.borrow();
        sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "OLDER", None)], 2).unwrap();
        assert_eq!(*api.puts.borrow(), puts, "not retried");
    }

    #[test]
    fn inbox_receives_pages_and_writes_lua() {
        let (_t, fs_) = setup();
        let api = Fake::default();
        api.pairings.borrow_mut().push(pairing("p1", ME, BRAN, 1_794_000_000));
        api.incoming.borrow_mut().extend([
            incoming(1, BRAN, ME, "partner", "OLD"),
            incoming(2, ME, ME, "self", "SELFFROMPC2"),
            incoming(3, BRAN, ME, "partner", "NEW"),
            incoming(4, BRAN, ME, "partner", "bad packet\"]] os.exit()"),
            incoming(5, "Player-6800-DEADBEEF", ME, "partner", "STRANGER"),
        ]);
        let mut st = load_state(&fs_);
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "C", None)], 1_794_770_100).unwrap();
        assert!(r.inbox_written);
        assert_eq!(st.since, 5);
        let me = &st.inbox.chars[ME];
        let from_bran: Vec<_> = me.packets.iter().filter(|p| p.from == BRAN).collect();
        assert_eq!(from_bran.len(), 1);
        assert_eq!(from_bran[0].data, "NEW");
        assert!(me.packets.iter().any(|p| p.kind == Kind::SelfCopy && p.data == "SELFFROMPC2"));
        assert!(!me.packets.iter().any(|p| p.data == "STRANGER"), "no live pairing, dropped");
        assert_eq!(me.pairs[0].my_key.as_deref(), Some("ABCDEFGHJKMNPQRS"));
        let lua = fs::read_to_string(fs_.root(Root::Companion).join("Inbox.lua")).unwrap();
        assert!(lua.contains("\"NEW\"") && !lua.contains("os.exit"));
        assert!(!companion::list_backups(&fs_).is_empty(), "backup before the inbox changed");
        let saved = load_state(&fs_);
        assert_eq!(saved.since, 5, "state committed");
    }

    #[test]
    fn removing_a_partner_in_game_ends_only_the_older_pairing() {
        let (_t, fs_) = setup();
        let api = Fake::default();
        api.pairings.borrow_mut().push(pairing("p1", ME, BRAN, 1_794_000_000));
        let mut st = load_state(&fs_);
        // Removal from before this pairing was made: ignored (they paired again since).
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "C", Some(1_793_000_000))], 1).unwrap();
        assert!(r.ended.is_empty());
        let r = sync(&api, &fs_, &mut st, &[outbox(1_794_770_000, "C", Some(1_794_500_000))], 2).unwrap();
        assert_eq!(r.ended, vec!["p1".to_string()]);
        let pr = &st.inbox.chars[ME].pairs[0];
        assert_eq!(pr.status, PairStatus::Ended);
        assert!(pr.my_key.is_none() && pr.partner_key.is_none(), "no keys once ended");
    }

    #[test]
    fn outbox_from_disk_flows_through() {
        let (_t, fs_) = setup();
        let text = format!(
            "SterlingQuestSyncOut = {{ [\"format\"] = 1, [\"characters\"] = {{ [\"{ME}\"] = {{ [\"name\"] = \"Kaelith Emberfall\", [\"realm\"] = \"CAMWOW US2 Realm 2 PvP\", [\"saved\"] = 1794770000, [\"proto\"] = 6, [\"partners\"] = {{ [\"{BRAN}\"] = {{ [\"packet\"] = \"ABC\", [\"change\"] = 1 }} }} }} }} }}"
        );
        let o = parse_outbox(&text).unwrap();
        let api = Fake::default();
        api.pairings.borrow_mut().push(pairing("p1", ME, BRAN, 1_794_000_000));
        let mut st = load_state(&fs_);
        let r = sync(&api, &fs_, &mut st, &[o], 1).unwrap();
        assert_eq!(r.uploaded, 1);
        assert_eq!(api.slots.borrow()[&slot_key(ME, BRAN)].1, "ABC");
    }
}
