// Placeholder page for the first builds. Text from the server is only ever set with textContent.
const call = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
const $ = (id) => document.getElementById(id);
const err = (e) => { $("err").textContent = e ? String(e) : ""; };
const ago = (t) => { if (!t) return "never"; const m = Math.round((Date.now() / 1000 - t) / 60); return m < 1 ? "just now" : m < 60 ? m + " min ago" : Math.round(m / 60) + " h ago"; };

function render(s) {
  $("ver").textContent = "v" + s.version;
  $("setup").hidden = !s.setup_needed;
  $("home").hidden = s.setup_needed;
  $("join").hidden = !s.game || s.joined;
  if (!$("label").value) $("label").value = s.pc_label;
  $("auto").checked = s.auto_sync;
  const online = s.online === false ? "Server offline" : "Server online";
  $("line").textContent = `${online} · last sync ${ago(s.last_sync)} · game ${s.game_running ? "running" : "closed"} · Sterling ${s.sterling_version || "not found"}`;
  const ul = $("chars"); ul.replaceChildren();
  for (const c of s.characters) {
    const li = document.createElement("li");
    li.textContent = `${c.name} · saved ${ago(c.saved)} · ${c.partners.length} partner(s) `;
    if (c.ready) { const b = document.createElement("span"); b.className = "ready"; b.textContent = "Ready"; li.append(b); }
    ul.append(li);
  }
  if (s.last_error) err(s.last_error);
}

async function refresh() { try { render(await call("status")); } catch (e) { err(e); } }

async function listGames() {
  const games = await call("discover_games");
  const box = $("games"); box.replaceChildren();
  games.forEach((g, i) => {
    const b = document.createElement("button");
    b.textContent = `Use ${g.flavour}${g.has_sterling ? " (Sterling found)" : ""}`;
    b.onclick = () => call("use_found_game", { index: i }).then(refresh, err);
    box.append(b);
  });
}

$("pick").onclick = () => call("pick_game_folder").then(refresh, err);
$("create").onclick = () => call("create_account", { label: $("label").value, invite: null }).then((j) => {
  if (j.recovery_code) { $("reccode").textContent = j.recovery_code; $("rec").hidden = false; $("join").hidden = false; }
}, err);
$("dolink").onclick = () => call("link_this_pc", { code: $("link").value, label: $("label").value }).then(refresh, err);
$("savetxt").onclick = () => call("save_recovery_txt", { code: $("reccode").textContent }).catch(err);
$("auto").onchange = (e) => call("set_auto_sync", { on: e.target.checked }).catch(err);
$("sync").onclick = () => { err(""); call("sync_now").then(refresh, err); };

window.__TAURI__.event.listen("status", (e) => render(e.payload));
refresh();
listGames().catch(err);
