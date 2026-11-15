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
  return S.characters.find((c) => c.guid === S.character) || S.characters[0];
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

async function loadGames() {
  gamesLoaded = true;
  const box = $("games"); box.replaceChildren(el("div", "small muted", "Looking for WoW..."));
  let games = [];
  try { games = await call("discover_games"); } catch (e) { fail(e); }
  box.replaceChildren();
  if (!games.length) box.append(el("div", "dashed", "Couldn't find WoW on its own. Pick the folder below."));
  // WoW Forever is sorted first, so it starts selected. Any other one can be picked instead.
  games.forEach((g, i) => {
    const l = el("label", "choice game" + (i === 0 ? " framed" : ""));
    const r = el("input"); r.type = "radio"; r.name = "game"; r.value = String(i); r.checked = i === 0;
    r.onchange = () => { for (const x of box.querySelectorAll(".choice")) x.classList.toggle("framed", x.querySelector("input").checked); };
    const t = el("span");
    const label = g.flavour === "_forever_" ? "WoW Forever (_forever_)" : g.flavour;
    t.append(el("span", "ct", label), el("span", "cd", g.has_sterling ? "Sterling found here" : "No Sterling here"));
    l.append(r, t);
    if (i > 0) l.hidden = true; // other game versions stay tucked away until asked for
    box.append(l);
  });
  if (games.length > 1) {
    const more = el("a", "small", "Show other game folders (" + (games.length - 1) + ")"); more.href = "#";
    more.onclick = (e) => { e.preventDefault(); for (const x of box.querySelectorAll(".choice")) x.hidden = false; more.remove(); };
    box.append(more);
  }
  $("folder-go").disabled = !games.length;
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
  else { sel.disabled = false; for (const ch of S.characters) { const o = el("option", "", ch.name); o.value = ch.guid; if (c && ch.guid === c.guid) o.selected = true; sel.append(o); } }
  $("thispc").textContent = "This PC: " + S.pc_label;
  for (const n of document.querySelectorAll(".charname")) n.textContent = c ? c.name : "your character";

  renderHome(c);
  if (page === "partners") renderPartners(c);
  if (page === "pair") renderPair(c);
  if (page === "activity") renderActivity();
  if (page === "settings") renderSettings();
}

function renderHome(c) {
  $("auto").checked = S.auto_sync;
  $("sync").disabled = S.busy || !c;
  $("sync").textContent = S.busy ? "Syncing..." : "Sync now";
  const dot = $("h-dot");
  if (!c) {
    $("h-char").textContent = "WELCOME";
    $("h-title").textContent = "Waiting for your first save";
    dot.className = "dot";
    $("h-line").textContent = S.companion_installed
      ? "Log in to WoW with Sterling enabled. Your characters show up here after you log out or /reload."
      : "The SterlingQuestSync add-on isn't in your AddOns folder. Pick your game folder again in Settings.";
  } else {
    $("h-char").textContent = c.name.toUpperCase();
    if (S.busy) { $("h-title").textContent = "Syncing..."; dot.className = "dot gold"; $("h-line").textContent = "Talking to the server."; }
    else if (S.last_error) { $("h-title").textContent = "Couldn't sync"; dot.className = "dot bad"; $("h-line").textContent = S.last_error; }
    else if (c.ready) { $("h-title").textContent = "Ready to sync"; dot.className = "dot gold"; $("h-line").textContent = "New save found. Saved " + ago(c.saved) + (S.auto_sync ? ". Syncing on its own." : "."); }
    else { $("h-title").textContent = "Up to date"; dot.className = "dot good"; $("h-line").textContent = c.saved ? "Nothing new since your last save, " + ago(c.saved) + "." : "Waiting for a save from this PC."; }
  }
  $("h-last").textContent = S.last_sync ? "Last synced " + ago(S.last_sync) : "Not synced yet";

  const card = $("sterling-card");
  const v = S.sterling_version, l = S.sterling_latest;
  if (v && l && newer(l, v)) {
    card.hidden = false;
    const t = $("sterling-text"); t.replaceChildren(el("b", "", "Sterling "), el("span", "muted", v + " to "), el("b", "good-t", l));
  } else card.hidden = true;

  const hp = $("h-partners"); hp.replaceChildren();
  const list = c ? c.partners.filter((p) => !hidden.has(p.pairing_id) && (p.active || p.ended_reason === "replaced")) : [];
  if (!list.length) {
    hp.append(el("div", "empty", "No partners yet."));
    const b = el("button", "btn small", "Pair a friend"); b.type = "button"; b.onclick = () => go("pair"); hp.append(b);
  }
  for (const p of list.slice(0, 5)) {
    const row = el("div", "li"); const left = el("div");
    left.append(el("div", "n", p.name));
    if (!p.active) {
      left.append(el("div", "small bad-t", "Missing key, resync"));
      const b = el("button", "btn small", "Resync"); b.type = "button"; b.onclick = () => go("pair");
      row.append(left, b);
    } else {
      left.append(el("div", "small muted", p.last_copy ? "Copy from " + ago(p.last_copy) : "No copy yet"));
      row.append(left, p.last_copy ? el("span", p.applied ? "small good-t" : "small muted", p.applied ? "In game" : "Waiting for /reload") : el("span"));
    }
    hp.append(row);
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

function statusCell(p) {
  const td = el("td");
  if (p.active) {
    td.append(el("div", p.last_copy ? "good-t" : "muted", p.last_copy ? (p.applied ? "Synced, in game" : "Synced, /reload to see it") : "Paired, no copy yet"));
  } else if (p.ended_reason === "replaced") {
    td.append(el("div", "bad-t", "Missing key, resync"), el("div", "small muted", "They reinstalled or paired again. Pair again with a new code."));
  } else if (p.ended_reason === "deleted") {
    td.append(el("div", "muted", "Account deleted"));
  } else {
    td.append(el("div", "muted", "Removed"));
  }
  return td;
}

function renderPartners(c) {
  $("p-sub").replaceChildren();
  if (c) $("p-sub").append("Syncing with ", el("b", "", c.name), ". " + liveCount(c) + " of 10.");
  const body = $("p-rows"); body.replaceChildren();
  const list = c ? c.partners.filter((p) => !hidden.has(p.pairing_id)) : [];
  if (!list.length) { const tr = el("tr"); const td = el("td", "empty", "No partners yet. Pair a friend to start."); td.colSpan = 4; tr.append(td); body.append(tr); }
  for (const p of list) {
    const tr = el("tr");
    const name = el("td"); name.append(el("div", p.active ? "n" : "n muted", p.name), el("div", "small muted", p.realm));
    const last = el("td", p.last_copy ? "" : "muted", p.last_copy ? ago(p.last_copy) : "None");
    const act = el("td");
    if (p.active) {
      const b = el("button", "btn small ghost", "Stop syncing"); b.type = "button";
      b.onclick = async () => {
        const ok = await dialog("Stop syncing with " + p.name + "?", [para("Their copies stop coming in and yours stop going out. You can pair again later with a new code.", "muted")], [["Cancel", false, "ghost"], ["Stop syncing", true, "bad"]]);
        if (ok) busy(b, () => call("end_pairing", { id: p.pairing_id }));
      };
      act.append(b);
    } else {
      if (p.ended_reason === "replaced") { const r = el("button", "btn small", "Resync"); r.type = "button"; r.onclick = () => go("pair"); act.append(r, " "); }
      const h = el("button", "btn small ghost", "Hide"); h.type = "button";
      h.onclick = () => { hidden.add(p.pairing_id); try { localStorage.setItem("hiddenPairs", JSON.stringify([...hidden])); } catch (_) {} renderApp(); };
      act.append(h);
    }
    tr.append(name, last, statusCell(p), act);
    body.append(tr);
  }
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
  $("s-sterling").textContent = S.sterling_version ? "Installed " + S.sterling_version + ". Your old copy is kept as a backup." : "Sterling isn't installed in this game folder.";
  $("s-path").textContent = S.game_path || "Not set";
  if (!pcsLoaded) loadPcs();
}

function go(p) { page = p; if (p === "settings") pcsLoaded = false; renderApp(); }

// ---------------------------------------------------------------- wiring

$("win-min").onclick = () => call("hide_to_tray");
$("win-close").onclick = () => call("quit");
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

$("pick").onclick = (e) => { e.preventDefault(); busy(null, async () => { await call("pick_game_folder"); setupStep = "first"; }); };
$("folder-go").onclick = () => {
  const r = document.querySelector("input[name=game]:checked");
  if (!r) return fail("Pick a game folder first.");
  busy($("folder-go"), async () => { await call("use_found_game", { index: Number(r.value) }); setupStep = "first"; });
};
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
$("s-sterling-auto").onchange = (e) => call("set_options", { autoUpdateSterling: e.target.checked }).catch(fail);
$("char").onchange = (e) => call("set_options", { character: e.target.value }).catch(fail);
$("sterling-update").onclick = () => busy($("sterling-update"), async () => { const v = await call("update_sterling"); toast("Sterling " + v + " installed" + (S.game_running ? ". /reload in game." : "")); });

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
$("s-change").onclick = () => busy($("s-change"), () => call("pick_game_folder"));
$("s-backups").onclick = () => call("open_backups").catch(fail);
$("s-delete").onclick = async () => {
  const ok = await dialog("Delete your data?", [para("This removes your account, every PC, your pairings and your copies from the server, right away. Copies already on friends' PCs stay there. Your quests in game are not touched.", "muted"), warnBox("This can't be undone", "To sync again you'd start a new account and pair with friends again.")], [["Cancel", false, "ghost"], ["Delete everything", true, "bad"]]);
  if (!ok) return;
  busy($("s-delete"), async () => { await call("delete_my_data"); toast("Your data was deleted from the server"); });
};

T.event.listen("status", (e) => render(e.payload));
call("status").then(render, fail);
