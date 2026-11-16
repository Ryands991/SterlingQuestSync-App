// Sterling Quest Sync window. Every value from the server or the game is shown with textContent,
// never as HTML. The page can only call the app's own typed commands.
"use strict";

const T = window.__TAURI__;
const call = (cmd, args) => T.core.invoke(cmd, args || {});
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };

let S = null;              // last status from the app
let page = "home";
let setupStep = "first";   // first | link | recover
let pendingRec = null;     // recovery code to show once after joining
const madeCodes = {};      // pair code id -> code text, only for codes made in this session
let hidden = new Set();
try { hidden = new Set(JSON.parse(localStorage.getItem("hiddenPairs") || "[]")); } catch (_) {}

// ---------------------------------------------------------------- helpers

function ago(t) {
  if (!t) return "never";
  const s = Math.max(0, Date.now() / 1000 - t);
  if (s < 60) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return m + " min ago";
  const h = Math.round(m / 60);
  if (h < 48) return h + (h === 1 ? " hr ago" : " hrs ago");
  const d = Math.round(h / 24);
  return d + " days ago";
}
function clock(t) {
  const d = new Date(t * 1000), now = new Date();
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return d.toDateString() === now.toDateString() ? time : d.toLocaleDateString([], { month: "short", day: "numeric" }) + " " + time;
}
function newer(a, b) {
  const p = (s) => String(s).split(".").map((x) => parseInt(x, 10) || 0);
  const x = p(a), y = p(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) { const d = (x[i] || 0) - (y[i] || 0); if (d) return d > 0; }
  return false;
}
let toastTimer;
function toast(text, bad) {
  const t = $("toast");
  t.textContent = String(text).replace(/^Error: /, "");
  t.className = "toast" + (bad ? " bad" : "");
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), bad ? 6000 : 2500);
}
const fail = (e) => toast(e, true);
async function busy(btn, fn) {
  if (btn) btn.disabled = true;
  try { return await fn(); } catch (e) { fail(e); } finally { if (btn) btn.disabled = false; }
}
async function copy(text) {
  try { await navigator.clipboard.writeText(text); toast("Copied"); } catch (_) { fail("Couldn't copy. Select the code and press Ctrl+C."); }
}
function currentChar() {
  if (!S || !S.characters.length) return null;
  return S.characters.find((c) => c.guid === S.character) || null; // never picked for you
}
function liveCount(c) { return c ? c.partners.filter((p) => p.active).length : 0; }

// ---------------------------------------------------------------- dialog

function dialog(title, body, buttons) {
  return new Promise((resolve) => {
    $("dlg-title").textContent = title;
    const b = $("dlg-body"); b.replaceChildren(...(Array.isArray(body) ? body : [body]).filter(Boolean));
    const row = $("dlg-btns"); row.replaceChildren();
    const close = (v) => { $("scrim").hidden = true; resolve(v); };
    for (const [label, value, cls] of buttons) {
      const btn = el("button", "btn " + (cls || ""), label);
      btn.type = "button";
      btn.onclick = () => close(value);
      row.append(btn);
    }
    $("scrim").hidden = false;
    $("scrim").onkeydown = (e) => { if (e.key === "Escape") close(null); };
    const last = row.lastElementChild; if (last) last.focus();
  });
}
const para = (text, cls) => el("p", cls || "", text);
function facts(rows) {
  const f = el("div", "facts");
  for (const [k, v] of rows) { const r = el("div"); r.append(el("span", "", k), el("span", "", v)); f.append(r); }
  return f;
}
function warnBox(title, text) {
  const w = el("div", "warn"); const d = el("span", "dot bad");
  const inner = el("div"); inner.append(el("div", "bad-t", title), el("div", "small", text));
  w.append(d, inner); return w;
}
function codeBox(code) {
  const box = el("div", "card framed coderow");
  const c = el("div", "bigcode", code);
  const b = el("button", "btn primary", "Copy"); b.type = "button"; b.onclick = () => copy(code);
  box.append(c, b); return box;
}

// ---------------------------------------------------------------- render

function render(s) {
  S = s;
  $("ver").textContent = "v" + s.version;
  $("s-ver").textContent = "v" + s.version;
  const inSetup = s.rejoin || s.setup_needed || pendingRec;
  $("setup").hidden = !inSetup;
  $("app").hidden = !!inSetup;
  if (inSetup) renderSetup(); else renderApp();
}

function showStep(id) {
  for (const sec of document.querySelectorAll(".step")) sec.hidden = sec.id !== "step-" + id;
}

let gamesLoaded = false;
function renderSetup() {
  for (const id of ["pcname1", "pcname2", "pcname3"]) if (!$(id).value) $(id).value = S.pc_label;
  if (S.rejoin) return showStep(setupStep === "first" ? "rejoin" : setupStep);
  if (pendingRec) { $("reccode").textContent = pendingRec; return showStep("code"); }
  if (!S.game) { if (!gamesLoaded) loadGames(); return showStep("folder"); }
  showStep(setupStep);
}

const FLAVOURS = { _forever_: "WoW Forever", _classic_era_: "WoW Classic Era", _classic_: "WoW Classic", _anniversary_: "WoW Anniversary", _retail_: "WoW Retail", _ptr_: "WoW test realm", _beta_: "WoW beta" };
let found = [];
function statusLine(dotCls, text) { const d = el("div"); d.append(el("span", "dot " + dotCls), el("span", "", text)); return d; }

function showCandidate(g) {
  $("cand-path").textContent = g.path;
  const st = $("cand-status"); st.replaceChildren();
  const forever = g.flavour === "_forever_";
  st.append(statusLine(forever ? "good" : "bad", FLAVOURS[g.flavour] || "Game folder: " + g.flavour));
  st.append(g.sterling_version ? statusLine("good", "Sterling " + g.sterling_version + " installed") : statusLine("", "Sterling isn't installed here"));
  const n = g.accounts.length;
  st.append(n ? statusLine("good", n + (n === 1 ? " WoW account found" : " WoW accounts found")) : statusLine("", "No WoW accounts yet. That's fine, log in once later."));
  $("cand-install-row").hidden = !!g.sterling_version;
  $("game-warn").hidden = forever;
  $("folder-go").disabled = false;
}

async function loadGames() {
  gamesLoaded = true;
  try { found = await call("discover_games"); } catch (e) { fail(e); found = []; }
  if (!found.length) {
    $("cand-path").textContent = "Couldn't find WoW on its own. Click Change to pick the folder.";
    $("folder-go").disabled = true;
  } else {
    showCandidate(await call("candidate_found", { index: 0 }));
  }
  const others = found.length - 1;
  $("cand-others").hidden = others < 1;
  $("cand-others").replaceChildren();
  if (others > 0) {
    const a = el("a", "", "Use one of those"); a.href = "#";
    a.onclick = (e) => { e.preventDefault(); $("cand-list").hidden = !$("cand-list").hidden; };
    $("cand-others").append("Found " + others + " other WoW " + (others === 1 ? "version" : "versions") + " on this PC. ", a);
  }
  const list = $("cand-list"); list.replaceChildren();
  found.forEach((g, i) => {
    const b = el("button"); b.type = "button";
    b.append(el("span", "", g.path), el("span", "pill", (FLAVOURS[g.flavour] || g.flavour) + (g.sterling_version ? " · Sterling" : "")));
    b.onclick = () => busy(null, async () => { showCandidate(await call("candidate_found", { index: i })); list.hidden = true; });
    list.append(b);
  });
}

function renderApp() {
  for (const b of document.querySelectorAll(".side > button")) {
    if (b.dataset.page === page) b.setAttribute("aria-current", "page"); else b.removeAttribute("aria-current");
  }
  for (const p of document.querySelectorAll(".page")) p.hidden = p.id !== "page-" + page;

  const srv = $("server");
  const dot = srv.querySelector(".dot");
  dot.className = "dot" + (S.online === false ? " bad" : S.online ? " good" : "");
  $("server-text").textContent = S.online === false ? "Server offline" : S.online ? "Server online" : "Checking server";

  const sel = $("char"); const c = currentChar();
  sel.replaceChildren();
  if (!S.characters.length) { const o = el("option", "", "No characters yet"); sel.append(o); sel.disabled = true; }
  else {
    sel.disabled = false;
    if (!c) { const o = el("option", "", "Select a character"); o.value = ""; o.disabled = true; o.selected = true; sel.append(o); }
    for (const ch of S.characters) { const o = el("option", "", ch.name); o.value = ch.guid; if (c && ch.guid === c.guid) o.selected = true; sel.append(o); } }
  $("thispc").textContent = "This PC: " + S.pc_label;
  for (const n of document.querySelectorAll(".charname")) n.textContent = c ? c.name : "your character";

  renderHome(c);
  if (page === "partners") renderPartners(c);
  if (page === "pair") renderPair(c);
  if (page === "activity") renderActivity();
  if (page === "settings") renderSettings();
}

function steps(list, head) {
  const o = $("h-steps"), h = $("h-steps-head");
  o.replaceChildren(...(list || []).map(t => el("li", "", t)));
  o.hidden = !list;
  h.textContent = head || "";
  h.hidden = !head;
}

function renderHome(c) {
  $("auto").checked = S.auto_sync;
  $("sync").disabled = S.busy || !c;
  $("sync").textContent = S.busy ? "Syncing..." : "Sync now";
  const dot = $("h-dot");
  steps(null);
  // No character chosen: Home shows only what to do next.
  $("home-rest").hidden = !c;
  $("hero-act").hidden = !c;
  $("h-last").hidden = !c;
  if (!c && S.characters.length) {
    $("h-char").textContent = "WELCOME";
    $("h-title").textContent = "Select a character to start";
    dot.className = "dot gold";
    $("h-line").textContent = "Pick one in the Character list at the bottom left.";
  } else if (!c) {
    $("h-char").textContent = "WELCOME";
    dot.className = "dot";
    if (!S.companion_installed) {
      $("h-title").textContent = "Add-on missing";
      $("h-line").textContent = "The SterlingQuestSync add-on isn't in your AddOns folder. Pick your game folder again in Settings.";
    } else if (!S.sterling_version) {
      $("h-title").textContent = "Sterling isn't installed";
      $("h-line").textContent = "Install Sterling Quest Tracker in this game folder. Your characters show up here once Sterling has something to share.";
    } else if (S.restart_game && S.game_running) {
      $("h-title").textContent = "Restart WoW";
      dot.className = "dot gold";
      $("h-line").textContent = "One more step: WoW needs a restart to find the new add-on.";
      steps([
        "Exit WoW completely. Logging out or /reload won't do it.",
        "Start WoW and log in to any character.",
        "That's it. Your character shows up here in a few seconds.",
      ]);
    } else {
      $("h-title").textContent = "Almost there";
      $("h-line").textContent = "Log in to WoW and your characters show up here.";
      steps([
        "Exit WoW completely and start it again. New add-ons only load when the game starts.",
        "At character select, click AddOns and make sure Sterling and Sterling Quest Sync are both ticked.",
        "Log in, then type /reload. Your character shows up when the game saves.",
        "Still nothing? In Settings, check the game folder is the WoW Forever folder you play from.",
      ], "Not showing up?");
    }
  } else {
    $("h-char").textContent = c.name.toUpperCase();
    if (S.busy) { $("h-title").textContent = "Syncing..."; dot.className = "dot gold"; $("h-line").textContent = "Talking to the server."; }
    else if (S.last_error) { $("h-title").textContent = "Couldn't sync"; dot.className = "dot bad"; $("h-line").textContent = S.last_error; }
    else if (c.ready) { $("h-title").textContent = "Ready to sync"; dot.className = "dot gold"; $("h-line").textContent = "New save found. Saved " + ago(c.saved) + (S.auto_sync ? ". Syncing on its own." : "."); }
    else { $("h-title").textContent = "Up to date"; dot.className = "dot good"; $("h-line").textContent = c.saved ? "Nothing new since your last save, " + ago(c.saved) + "." : "Waiting for a save from this PC."; }
  }
  $("h-last").textContent = S.last_sync ? "Last synced " + ago(S.last_sync) : "Not synced yet";

  const ac = $("app-card");
  const av = S.app_update || (S.app_latest && newer(S.app_latest, S.version) ? S.app_latest : null);
  const up = S.updating;
  $("app-progress").hidden = !up;
  if (up) {
    ac.hidden = false;
    $("app-text").replaceChildren(el("b", "", "Updating Sterling Quest Sync"), el("span", "muted", av ? " to " : ""), el("b", "good-t", av || ""));
    $("app-update").hidden = true;
    $("app-step").textContent = up.percent != null ? up.step + ": " + up.percent + "%" : up.step;
    const bar = $("app-fill").parentElement;
    bar.classList.toggle("wait", up.percent == null);
    $("app-fill").style.width = up.percent != null ? up.percent + "%" : "";
  } else if (av) {
    ac.hidden = false;
    $("app-text").replaceChildren(el("b", "", "Sterling Quest Sync "), el("span", "muted", S.version + " to "), el("b", "good-t", av));
    $("app-update").hidden = !S.app_update; // only offer the button once a signed update is ready
  } else ac.hidden = true;

  const card = $("sterling-card");
  const v = S.sterling_version, l = S.sterling_latest;
  if (!v && S.game) {
    card.hidden = false;
    $("sterling-text").replaceChildren(el("b", "", "Sterling Quest Tracker "), el("span", "muted", "isn't installed in this game folder"));
    $("sterling-update").textContent = "Install";
  } else if (v && l && newer(l, v)) {
    card.hidden = false;
    $("sterling-text").replaceChildren(el("b", "", "Sterling "), el("span", "muted", v + " to "), el("b", "good-t", l));
    $("sterling-update").textContent = "Update";
  } else card.hidden = true;

  // Same rows as the Partners page: favourites (up to 5), or the first few until some are starred.
  const hp = $("h-partners"); hp.replaceChildren();
  const all = c ? partnerRows(c) : [];
  const favs = all.filter((r) => r.fav);
  if (!all.length) {
    hp.append(el("div", "empty", c ? "No partners yet." : "Select a character to start."));
    if (c) { const b = el("button", "btn small", "Pair a friend"); b.type = "button"; b.onclick = () => go("pair"); hp.append(b); }
  } else {
    const box = el("div", "plist flat");
    for (const r of (favs.length ? favs : all.slice(0, 3))) box.append(prow(r.opts));
    hp.append(box);
    const foot = el("div", "row spread small muted hfoot");
    foot.append(el("span", "", favs.length ? "" : "Star partners to keep them here."));
    const more = el("button", "btn small ghost", "All partners (" + all.length + ")"); more.type = "button"; more.onclick = () => go("partners");
    foot.append(more); hp.append(foot);
  }

  const hr = $("h-recent"); hr.replaceChildren();
  if (!S.activity.length) hr.append(el("div", "empty", "Nothing yet."));
  for (const a of S.activity.slice(0, 5)) hr.append(eventRow(a));
}

function eventRow(a) {
  const r = el("div", "ev");
  r.append(el("span", "t", clock(a.t)), el("span", a.tone === "bad" ? "bad-t" : "", a.text));
  return r;
}

const CLASS_NAMES = { WARRIOR: "Warrior", PALADIN: "Paladin", HUNTER: "Hunter", ROGUE: "Rogue", PRIEST: "Priest", SHAMAN: "Shaman", MAGE: "Mage", WARLOCK: "Warlock", DRUID: "Druid", DEATHKNIGHT: "Death Knight", MONK: "Monk", DEMONHUNTER: "Demon Hunter", EVOKER: "Evoker" };
const classCls = (look) => (look && CLASS_NAMES[look.class] ? " c-" + look.class : "");

// One row in Sterling's in-game style: name, status line, right side, menu.
function prow(o) {
  const row = el("div", "prow" + (o.you ? " you" : ""));
  if (o.you) row.append(el("span", "ptag", "YOU"));
  const dim = !!o.dim;
  const m = el("div", "pm");
  // First name (Sterling's nickname) on top, character name under it, like the list in game.
  const nick = o.look && o.look.nick;
  m.append(el("div", "pn" + (dim ? " dim" : classCls(o.look)), nick || o.name || "Sterling partner"));
  const line = el("div", "pl");
  const bits = [];
  if (o.status) bits.push(el("span", o.statusCls || "", o.status));
  if (nick && o.name && nick !== o.name) bits.push(o.name);
  if (o.look && o.look.level) bits.push("Lvl " + o.look.level);
  for (const b of o.extra || []) if (b) bits.push(b);
  // Each piece stays whole; a narrow list wraps only between pieces.
  const piece = (b, extra) => { const w = el("span", "bit" + (extra || "")); w.append(b); return w; };
  bits.forEach((b, i) => { if (i) line.append(" · "); line.append(piece(b)); });
  // In a narrow list (Home) the right column folds into this line; see .pl-time in the stylesheet.
  if (o.right && o.right[1] && !o.you) { const t = el("span", "pl-time"); t.append(" · ", piece(o.right[1])); line.append(t); }
  m.append(line);
  row.append(m);
  const r = el("div", "pr");
  if (o.right) { if (o.right[0]) r.append(el("div", "s " + (o.rightCls || ""), o.right[0])); if (o.right[1]) r.append(o.right[1]); }
  row.append(r);
  if (o.star) {
    const on = o.star.on;
    const b = el("button", "ico star" + (on ? " on" : ""), on ? "\u2605" : "\u2606"); b.type = "button";
    b.title = on ? "Remove from favourites" : "Add to favourites";
    b.setAttribute("aria-pressed", on ? "true" : "false");
    b.onclick = () => busy(b, async () => {
      const ok = await call("set_favorite", { partner: o.star.guid, on: !on });
      if (ok === false) toast("You can favourite up to 5 partners.", true);
    });
    row.append(b);
  }
  else if (o.menu && !o.you) row.append(el("span", "ico-gap"));
  if (o.menu) {
    const b = el("button", "ico", "⋯"); b.type = "button"; b.title = "More"; b.setAttribute("aria-label", "More for " + (o.name || "partner"));
    b.onclick = o.menu; row.append(b);
  }
  return row;
}

// Last time anything synced with them: a copy through the app, or a live sync in game.
function lastTime(...ts) {
  const t = Math.max(0, ...ts.filter(Boolean));
  return t ? "synced " + ago(t) : "not synced yet";
}

// Every partner row for a character: favourites first (in the order starred), then live pairs,
// Sterling partners not paired in the app yet, and ended pairs.
function partnerRows(c) {
  const favs = c.favs || [];
  const star = (guid) => ({ guid, on: favs.includes(guid) });
  const rows = [];
  const unpaired = new Set(c.unpaired.map((u) => u.guid));
  const list = c.partners.filter((p) => !hidden.has(p.pairing_id) && !(!p.active && unpaired.has(p.guid)));
  for (const p of list.filter((p) => p.active)) {
    rows.push({ guid: p.guid, opts: {
      name: p.name, look: p.look, star: star(p.guid),
      status: p.last_copy ? (p.applied ? "Syncing" : "Syncing, /reload to see it") : "Paired. Waiting for their next save",
      statusCls: p.last_copy ? "s-ok" : "",
      right: [null, lastTime(p.last_copy, p.look && p.look.seen)],
      menu: async () => {
        const ok = await dialog("Stop syncing with " + ((p.look && p.look.nick) || p.name) + "?", [para("Their copies stop coming in and yours stop going out. You can pair again later with a new code.", "muted")], [["Cancel", false, "ghost"], ["Stop syncing", true, "bad"]]);
        if (ok) busy(null, () => call("end_pairing", { id: p.pairing_id }));
      },
    } });
  }
  for (const u of c.unpaired) {
    const [status, cls] = u.offered ? ["Pairing on its own", "s-wait"]
      : u.app === "yes" ? ["Has the app, not paired", "s-wait"]
      : u.app === "no" ? ["Doesn't have the app yet", "s-bad"]
      : ["Not seen yet", ""];
    rows.push({ guid: u.guid, opts: {
      name: u.name, look: u.look, status, statusCls: cls, star: star(u.guid), right: [null, lastTime(u.look && u.look.seen)],
      menu: async () => {
        const go2 = await dialog((u.look && u.look.nick) || u.name || "Sterling partner", [para(u.app === "no" ? "They need Sterling Quest Sync installed first. Once they have it, pairing happens on its own when you're both online in game." : "This usually pairs on its own when you're both online in game. You can also pair now with a code.", "muted")], [["Close", false, "ghost"], ["Pair a friend", true]]);
        if (go2) go("pair");
      },
    } });
  }
  for (const p of list.filter((p) => !p.active)) {
    const replaced = p.ended_reason === "replaced";
    const why = replaced ? "Missing key, resync" : p.ended_reason === "deleted" ? "Account deleted" : "Stopped syncing";
    rows.push({ guid: p.guid, opts: {
      name: p.name, look: p.look, dim: !replaced,
      status: why, statusCls: replaced ? "s-bad" : "",
      right: [null, p.ended_at ? "ended " + ago(p.ended_at) : ""],
      menu: async () => {
        const btns = [["Close", null, "ghost"], ["Hide", "hide"]];
        if (replaced) btns.push(["Pair again", "pair"]);
        const v = await dialog((p.look && p.look.nick) || p.name, [para(replaced ? "They reinstalled or paired again from somewhere else. Pair again with a new code." : "This pairing has ended. Hide it from the list, or pair again any time with a new code.", "muted")], btns);
        if (v === "pair") go("pair");
        if (v === "hide") { hidden.add(p.pairing_id); try { localStorage.setItem("hiddenPairs", JSON.stringify([...hidden])); } catch (_) {} renderApp(); }
      },
    } });
  }
  for (const r of rows) r.fav = !!(r.opts.star && r.opts.star.on);
  const rank = (r) => (r.fav ? favs.indexOf(r.guid) : 99);
  return rows.map((r, i) => [r, i]).sort((a, b) => rank(a[0]) - rank(b[0]) || a[1] - b[1]).map((x) => x[0]);
}

function renderPartners(c) {
  $("p-sub").replaceChildren();
  const you = $("p-you"); you.replaceChildren();
  const body = $("p-rows"); body.replaceChildren();
  // Same count as Sterling's list in game: every current partner, paired in the app or not.
  $("p-count").textContent = c ? (c.partners.filter((p) => p.active).length + c.unpaired.length) + " / 10" : "";
  if (!c) { body.append(el("div", "empty", "Select a character to start.")); return; }
  const linked = c.partners.filter((p) => p.active).length;
  const inSterling = c.unpaired.length + linked;
  $("p-sub").append(inSterling ? linked + " of " + inSterling + " syncing through the app. Star up to 5 favourites; they stay on top here, on Home and in game." : "Syncing with nobody yet.");

  const look = c.look || {};
  const race = [look.race, CLASS_NAMES[look.class]].filter(Boolean).join(" ");
  you.append(prow({
    you: true, name: c.name, look,
    extra: [race, (c.sterling || S.sterling_version) ? "Sterling " + (c.sterling || S.sterling_version) : "", "App " + S.version],
    right: c.ready ? ["Ready to sync", "save waiting"] : ["Up to date", S.last_sync ? "synced " + ago(S.last_sync) : ""],
    rightCls: c.ready ? "s-wait" : "s-ok",
  }));

  const rows = partnerRows(c);
  if (!rows.length) body.append(el("div", "empty", "No partners yet. Pair a friend to start."));
  for (const r of rows) body.append(prow(r.opts));
}

let codesFor = null, openCodes = [], latestId = null;
async function loadCodes(c) {
  if (!c) return;
  try { openCodes = (await call("open_codes")).filter((x) => x.guid === c.guid); } catch (e) { openCodes = []; }
  codesFor = c.guid;
  drawCodes(c);
}
function drawCodes(c) {
  const latest = latestId && openCodes.find((x) => x.id === latestId);
  $("give-empty").hidden = !!latest;
  $("give-made").hidden = !latest;
  if (latest) $("latest-code").textContent = madeCodes[latest.id];
  const others = openCodes.filter((x) => !latest || x.id !== latest.id);
  $("older").hidden = !others.length;
  const ol = $("older-list"); ol.replaceChildren();
  for (const o of others) {
    const r = el("div", "li");
    r.append(el("span", madeCodes[o.id] ? "mono" : "small muted", madeCodes[o.id] || "Code made earlier, ends " + clock(o.expires_at)));
    const b = el("button", "btn small ghost", "Cancel"); b.type = "button";
    b.onclick = () => busy(b, async () => { await call("cancel_code", { id: o.id }); await loadCodes(c); });
    r.append(b); ol.append(r);
  }
  $("pair-count").replaceChildren();
  $("pair-count").append(c.name + " has " + liveCount(c) + " of 10 partners. Need more than 10? ");
  const a = el("a", "", "Let us know"); a.href = "#"; a.onclick = (e) => { e.preventDefault(); call("open_page", { which: "issues" }); };
  $("pair-count").append(a, ".");
}
function renderPair(c) {
  $("gen").disabled = !c; $("check-code").disabled = !c;
  if (!c) { $("pair-count").textContent = "Log in on a character first, then come back here."; return; }
  if (codesFor !== c.guid) { latestId = null; loadCodes(c); } else drawCodes(c);
}
async function makeCode(btn) {
  const c = currentChar(); if (!c) return;
  await busy(btn, async () => {
    const r = await call("make_pair_code", { guid: c.guid });
    madeCodes[r.id] = r.code; latestId = r.id;
    await loadCodes(c);
  });
}

function renderActivity() {
  const l = $("act-list"); l.replaceChildren();
  if (!S.activity.length) l.append(el("div", "empty", "Nothing yet."));
  for (const a of S.activity) l.append(eventRow(a));
}

let pcsLoaded = false;
async function loadPcs() {
  pcsLoaded = true;
  const box = $("pcs");
  try {
    const a = await call("account");
    box.replaceChildren();
    for (const pc of a.pcs) {
      const r = el("div", "li");
      const left = el("span"); left.append(el("span", "n", pc.label), pc.current ? el("span", "muted", " (this PC)") : "");
      if (pc.current) r.append(left, el("span", "small good-t", "Active now"));
      else {
        const right = el("span", "row"); right.append(el("span", "small muted", "Last seen " + ago(pc.last_seen)));
        const b = el("button", "btn small ghost", "Remove"); b.type = "button";
        b.onclick = async () => {
          const ok = await dialog("Remove " + pc.label + "?", [para("That PC stops syncing until it's linked again. Nothing on it is deleted.", "muted")], [["Cancel", false, "ghost"], ["Remove", true, "bad"]]);
          if (ok) busy(b, async () => { await call("remove_pc", { id: pc.id }); loadPcs(); });
        };
        right.append(b); r.append(left, right);
      }
      box.append(r);
    }
  } catch (e) { box.replaceChildren(el("div", "small muted", "Couldn't load your PCs: " + e)); }
}
function renderSettings() {
  $("s-auto").checked = S.auto_sync;
  $("s-sterling-auto").checked = S.auto_update_sterling;
  $("s-app-auto").checked = S.auto_update_app;
  $("s-ver").textContent = S.version;
  $("s-tray").checked = S.close_to_tray;
  $("s-sterling").textContent = S.sterling_version ? "Installed " + S.sterling_version + ". Your old copy is kept as a backup." : "Sterling isn't installed in this game folder.";
  $("s-path").textContent = S.game_path || "Not set";
  if (!pcsLoaded) loadPcs();
}

function go(p) { page = p; if (p === "settings") pcsLoaded = false; renderApp(); }

// ---------------------------------------------------------------- wiring

$("win-min").onclick = () => T.window.getCurrentWindow().minimize().catch(fail);
$("win-close").onclick = () => call("close_window");
for (const b of document.querySelectorAll("[data-page]")) b.addEventListener("click", () => go(b.dataset.page));
for (const b of document.querySelectorAll("[data-copy]")) b.addEventListener("click", () => copy($(b.dataset.copy).textContent));
for (const a of document.querySelectorAll("[data-open]")) a.addEventListener("click", (e) => { e.preventDefault(); call("open_page", { which: a.dataset.open }); });
for (const b of document.querySelectorAll("[data-back]")) b.addEventListener("click", () => {
  if (b.dataset.back === "folder") { gamesLoaded = false; showStep("folder"); loadGames(); return; }
  setupStep = "first"; if (S) render(S);
});
for (const r of document.querySelectorAll("input[name=first]")) r.addEventListener("change", () => {
  for (const l of document.querySelectorAll("#step-first .choice")) l.classList.toggle("framed", l.querySelector("input").checked);
});

const OTHER_WOW = ["_classic_era_", "_classic_", "_anniversary_", "_retail_", "_ptr_", "_beta_", "_classic_ptr_", "_xptr_"];
async function warnIfNotForever(g) {
  if (g && OTHER_WOW.includes(g.flavour)) {
    await dialog("Made for WoW Forever", [para("You picked " + g.flavour + ". Sterling Quest Sync and Sterling are built and tested for WoW Forever. With another version of WoW they may not work as expected.", "muted"), para("You can switch back any time in Settings, under Game folder.", "small muted")], [["OK", true, "primary"]]);
  }
}
$("cand-change").onclick = () => busy($("cand-change"), async () => { const g = await call("candidate_pick"); if (g) showCandidate(g); });
$("folder-go").onclick = () => busy($("folder-go"), async () => {
  const install = !$("cand-install-row").hidden && $("cand-install").checked;
  if (install) $("folder-go").textContent = "Installing Sterling...";
  try { await call("confirm_game", { installSterling: install }); } finally { $("folder-go").textContent = "Continue"; }
  if (S.game_running) toast("WoW is open. Exit it completely and start it again so it finds the new add-on.");
  setupStep = "first";
});
$("to-recover").onclick = (e) => { e.preventDefault(); setupStep = "recover"; render(S); };
$("first-go").onclick = () => {
  const yes = document.querySelector("input[name=first]:checked").value === "yes";
  if (!yes) { $("pcname2").value = $("pcname1").value; setupStep = "link"; return render(S); }
  busy($("first-go"), async () => { const j = await call("create_account", { label: $("pcname1").value, invite: null }); pendingRec = j.recovery_code; render(await call("status")); });
};
$("link-go").onclick = () => busy($("link-go"), async () => { await call("link_this_pc", { code: $("linkcode").value, label: $("pcname2").value }); setupStep = "first"; render(await call("status")); });
$("recover-go").onclick = () => busy($("recover-go"), async () => { const j = await call("recover_account", { code: $("reccode-in").value, label: $("pcname3").value }); pendingRec = j.recovery_code; setupStep = "first"; render(await call("status")); });
$("rec-txt").onclick = () => busy($("rec-txt"), async () => { if (await call("save_recovery_txt", { code: pendingRec })) toast("Saved"); });
const recDone = () => { pendingRec = null; $("reccode").textContent = ""; page = "home"; render(S); };
$("rec-skip").onclick = recDone;
$("rec-done").onclick = recDone;
$("rj-fresh").onclick = () => busy($("rj-fresh"), async () => { const j = await call("create_account", { label: S.pc_label, invite: null }); pendingRec = j.recovery_code; render(await call("status")); });
$("rj-link").onclick = () => { setupStep = "link"; render(S); };
$("rj-recover").onclick = () => { setupStep = "recover"; render(S); };

$("sync").onclick = () => call("sync_now").catch(fail);
$("auto").onchange = (e) => call("set_options", { autoSync: e.target.checked }).catch(fail);
$("s-auto").onchange = (e) => call("set_options", { autoSync: e.target.checked }).catch(fail);
$("s-tray").onchange = (e) => call("set_options", { closeToTray: e.target.checked }).catch(fail);
$("s-app-auto").onchange = (e) => call("set_options", { autoUpdateApp: e.target.checked }).catch(fail);
$("s-check").onclick = () => busy($("s-check"), async () => {
  $("s-check").textContent = "Checking...";
  try {
    const v = await call("check_for_app_update");
    $("s-ver-note").textContent = v ? "Version " + v + " is ready. Update it from Home." : "You have the newest version.";
    toast(v ? "Version " + v + " is ready. Update it from Home." : "You have the newest version.");
  } finally { $("s-check").textContent = "Check for updates"; }
});
$("app-update").onclick = async () => {
  const ok = await dialog("Update Sterling Quest Sync?", [para("The app closes for a few seconds, installs version " + S.app_update + " and opens again. Your settings and partners stay.", "muted")], [["Not now", false, "ghost"], ["Update", true, "primary"]]);
  if (ok) busy($("app-update"), () => call("update_app"));
};
$("s-sterling-auto").onchange = (e) => call("set_options", { autoUpdateSterling: e.target.checked }).catch(fail);
$("char").onchange = (e) => call("set_options", { character: e.target.value }).catch(fail);
$("sterling-update").onclick = () => busy($("sterling-update"), async () => { const v = await call("update_sterling"); toast("Sterling " + v + " installed" + (S.game_running ? (S.restart_game ? ". Exit WoW completely and start it again to load it." : ". Type /reload in game to load it.") : "")); });

$("gen").onclick = () => makeCode($("gen"));
$("gen2").onclick = () => makeCode($("gen2"));
$("cancel-latest").onclick = () => busy($("cancel-latest"), async () => { const c = currentChar(); await call("cancel_code", { id: latestId }); latestId = null; await loadCodes(c); });
$("check-code").onclick = () => busy($("check-code"), async () => {
  const c = currentChar(); const code = $("paircode").value.trim();
  if (!code) throw "Type the code your friend sent.";
  const pk = await call("peek_code", { guid: c.guid, code });
  if (pk.already_paired) { await dialog("Already paired", [para("You already sync " + c.name + " with " + pk.partner.name + ". Nothing to do.")], [["OK", true, "primary"]]); $("paircode").value = ""; return; }
  const body = [facts([["Their character", pk.partner.name], ["Realm", pk.partner.realm], ["Your character", c.name]])];
  if (pk.different_install) body.push(warnBox("You already sync with " + pk.partner.name, "This code comes from a different install. Check with them before accepting. If they reinstalled, this is expected."));
  body.push(para(pk.different_install ? "Accepting replaces the old pairing, sets new keys, and sends a full copy both ways." : "You'll each get the other's latest copy after your next save.", "small muted"));
  const ok = await dialog("Pair with " + pk.partner.name + "?", body, [["Cancel", false, "ghost"], [pk.different_install ? "Replace and pair" : "Pair", true, "primary"]]);
  if (!ok) return;
  await call("redeem_code", { guid: c.guid, code, replace: !!pk.different_install });
  $("paircode").value = "";
  toast("Paired with " + pk.partner.name);
  go("partners");
});

$("link-pc").onclick = () => busy($("link-pc"), async () => {
  const r = await call("link_code");
  await dialog("Link another PC", [para("On the other PC, install Sterling Quest Sync and choose \"No, I already use it on another PC\". Type this code there.", "muted"), codeBox(r.code), para("Lasts 10 minutes and works once.", "small muted")], [["Done", true, "primary"]]);
  pcsLoaded = false; renderApp();
});
$("new-rec").onclick = async () => {
  const ok = await dialog("Make a new recovery code?", [para("Your old recovery code stops working right away.", "muted")], [["Cancel", false, "ghost"], ["Make a new code", true, "primary"]]);
  if (!ok) return;
  busy($("new-rec"), async () => {
    const code = await call("new_recovery_code");
    const dl = el("button", "btn", "Download .txt"); dl.type = "button";
    dl.onclick = () => call("save_recovery_txt", { code }).then((saved) => saved && toast("Saved"), fail);
    await dialog("Your new recovery code", [para("Keep it somewhere safe, like a password manager. Shown once.", "muted"), codeBox(code), dl], [["I saved it", true, "primary"]]);
  });
};
$("s-change").onclick = () => busy($("s-change"), async () => warnIfNotForever(await call("pick_game_folder")));
$("s-backups").onclick = () => call("open_backups").catch(fail);
$("s-delete").onclick = async () => {
  const ok = await dialog("Delete your data?", [para("This removes your account, every PC, your pairings and your copies from the server, right away. Copies already on friends' PCs stay there. Your quests in game are not touched.", "muted"), warnBox("This can't be undone", "To sync again you'd start a new account and pair with friends again.")], [["Cancel", false, "ghost"], ["Delete everything", true, "bad"]]);
  if (!ok) return;
  busy($("s-delete"), async () => { await call("delete_my_data"); toast("Your data was deleted from the server"); });
};

T.event.listen("status", (e) => render(e.payload));
call("status").then(render, fail);
