// Sterling Quest Sync Companion window. Every value from the server or the game is shown with textContent,
// never as HTML. The page can only call the app's own typed commands.
"use strict";

const tauri = window.__TAURI__;
const call = (cmd, args) => tauri.core.invoke(cmd, args || {});
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) =>
{
    const element = document.createElement(tag);

    if (cls) element.className = cls;
    if (text != null) element.textContent = text;

    return element;
};

let state = null;              // last status from the app
let page = "home";
let setupStep = "first";   // first | link | recover
let pendingRec = null;     // recovery code to show once after joining
const madeCodes = {};      // pair code id -> code text, only for codes made in this session
let hidden = new Set();

try
{
    hidden = new Set(JSON.parse(localStorage.getItem("hiddenPairs") || "[]"));
}

catch (_) {}

// ---------------------------------------------------------------- helpers

// How long ago a Unix time was, in words.
function ago(time)
{
    if (!time) return "never";

    const seconds = Math.max(0, Date.now() / 1000 - time);

    if (seconds < 60) return "just now";

    const minutes = Math.round(seconds / 60);

    if (minutes < 60) return minutes + " min ago";

    const hours = Math.round(minutes / 60);

    if (hours < 48) return hours + (hours === 1 ? " hr ago" : " hrs ago");

    const days = Math.round(hours / 24);

    return days + " days ago";
}

// A Unix time as a clock time, with the day when it isn't today.
function clock(stamp)
{
    const date = new Date(stamp * 1000), now = new Date();
    const time = date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

    return date.toDateString() === now.toDateString() ? time : date.toLocaleDateString([], { month: "short", day: "numeric" }) + " " + time;
}

// Is the candidate version newer than the current one?
function newer(candidate, current)
{
    const parts = (version) => String(version).split(".").map((part) => parseInt(part, 10) || 0);
    const left = parts(candidate), right = parts(current);

    for (let i = 0; i < Math.max(left.length, right.length); i++)
    {
        const diff = (left[i] || 0) - (right[i] || 0);

        if (diff) return diff > 0;
    }

    return false;
}

let toastTimer;

// A short message at the bottom of the window.
function toast(text, bad)
{
    const box = $("toast");

    box.textContent = String(text).replace(/^Error: /, "");
    box.className = "toast" + (bad ? " bad" : "");
    box.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (box.hidden = true), bad ? 6000 : 2500);
}

const fail = (error) => toast(error, true);

// Runs a job with its button disabled, and shows any error.
async function busy(btn, work)
{
    if (btn) btn.disabled = true;

    try
    {
        return await work();
    }

    catch (error)
    {
        fail(error);
    }

    finally
    {
        if (btn) btn.disabled = false;
    }
}

// Copies text to the clipboard and says so.
async function copy(text, done)
{
    try
    {
        await navigator.clipboard.writeText(text);

        toast(done || "Copied");
    }

    catch (_)
    {
        fail("Couldn't copy. Select the text and press Ctrl+C.");
    }
}

// The invite a player can send a friend.
function inviteText()
{
    return [
        "Want our quests to sync even when we play at different times? Get the Sterling Quest Sync Companion app (free, Windows, optional):",
        "https://github.com/SterlingAddons/SterlingQuestSync-App/releases/latest",
        "",
        "1. Download SterlingQuestSync_<version>_x64-setup.exe and run it. If Windows says \"Windows protected your PC\", click More info, then Run anyway.",
        "2. The app finds your WoW Forever folder and sets everything up. No email or password.",
        "3. Exit WoW completely and start it again (/reload isn't enough), then log in.",
        "",
        "We'll pair on our own the next time we're both online.",
    ].join("\n");
}

// The character picked in the menu, or null.
function currentChar()
{
    if (!state || !state.characters.length) return null;

    return state.characters.find((character) => character.guid === state.character) || null; // never picked for you
}

// How many partners a character syncs with right now.
function liveCount(character)
{
    return character ? character.partners.filter((partner) => partner.active).length : 0;
}

// ---------------------------------------------------------------- dialog

// A dialog with buttons; resolves with the button's value, or null on Escape.
function dialog(title, body, buttons)
{
    return new Promise((resolve) =>
    {
        $("dlg-title").textContent = title;

        const dialogBody = $("dlg-body");

        dialogBody.replaceChildren(...(Array.isArray(body) ? body : [body]).filter(Boolean));

        const row = $("dlg-btns");

        row.replaceChildren();

        const close = (choice) =>
        {
            $("scrim").hidden = true;
            resolve(choice);
        };

        for (const [label, value, cls] of buttons)
        {
            const btn = el("button", "btn " + (cls || ""), label);

            btn.type = "button";
            btn.onclick = () => close(value);
            row.append(btn);
        }

        $("scrim").hidden = false;
        $("scrim").onkeydown = (event) =>
        {
            if (event.key === "Escape") close(null);
        };

        const last = row.lastElementChild;

        if (last) last.focus();
    });
}

const para = (text, cls) => el("p", cls || "", text);

// A two-column list of labels and values.
function facts(rows)
{
    const factsBox = el("div", "facts");

    for (const [name, value] of rows)
    {
        const row = el("div");

        row.append(el("span", "", name), el("span", "", value));
        factsBox.append(row);
    }

    return factsBox;
}

// A red warning with a title and text.
function warnBox(title, text)
{
    const warning = el("div", "warn");
    const dot = el("span", "dot bad");
    const inner = el("div");

    inner.append(el("div", "bad-t", title), el("div", "small", text));

    warning.append(dot, inner);

    return warning;
}

// A big code with a Copy button.
function codeBox(code)
{
    const box = el("div", "card framed coderow");
    const bigCode = el("div", "bigcode", code);
    const copyButton = el("button", "btn primary", "Copy");

    copyButton.type = "button";
    copyButton.onclick = () => copy(code);

    box.append(bigCode, copyButton);

    return box;
}

// ---------------------------------------------------------------- render

let whatsNewShown = false;

// Shows what changed since the last version this PC saw.
function showWhatsNew()
{
    if (whatsNewShown || !state.whats_new || !state.whats_new.length || !$("scrim").hidden) return;

    whatsNewShown = true;

    const body = [];

    for (const [version, lines] of state.whats_new)
    {
        body.push(el("div", "eyebrow", "VERSION " + version));

        const ul = el("ul", "notes");

        for (const note of lines) ul.append(el("li", "", note));

        body.push(ul);
    }

    dialog("What's new", body, [["Got it", true, "primary"]]).then(() => call("dismiss_whats_new").catch(() => {}));
}

// Takes a new status from the app and redraws.
function render(status)
{
    state = status;
    $("ver").textContent = "v" + status.version;
    $("s-ver").textContent = "v" + status.version;

    const inSetup = status.rejoin || status.setup_needed || pendingRec;

    $("setup").hidden = !inSetup;
    $("app").hidden = !!inSetup;

    if (inSetup) renderSetup(); else
    {
        renderApp();
        showWhatsNew();
    }
}

// Shows one setup step.
function showStep(id)
{
    for (const sec of document.querySelectorAll(".step")) sec.hidden = sec.id !== "step-" + id;
}

let gamesLoaded = false;

// Picks the setup step that fits where this PC is.
function renderSetup()
{
    for (const id of ["pcname1", "pcname2", "pcname3"]) if (!$(id).value) $(id).value = state.pc_label;

    if (state.rejoin) return showStep(setupStep === "first" ? "rejoin" : setupStep);
    if (pendingRec)
    {
        $("reccode").textContent = pendingRec;

        return showStep("code");
    }

    if (!state.game)
    {
        if (!gamesLoaded) loadGames();

        return showStep("folder");
    }

    showStep(setupStep);
}

const FLAVOURS = { _forever_: "WoW Forever", _classic_era_: "WoW Classic Era", _classic_: "WoW Classic", _anniversary_: "WoW Anniversary", _retail_: "WoW Retail", _ptr_: "WoW test realm", _beta_: "WoW beta" };
let found = [];

// A colored dot with a line of text.
function statusLine(dotCls, text)
{
    const row = el("div");

    row.append(el("span", "dot " + dotCls), el("span", "", text));

    return row;
}

// Shows the game folder that was found or picked, and what's in it.
function showCandidate(candidate)
{
    $("cand-path").textContent = candidate.path;

    const statusBox = $("cand-status");

    statusBox.replaceChildren();

    const forever = candidate.flavour === "_forever_";

    statusBox.append(statusLine(forever ? "good" : "bad", FLAVOURS[candidate.flavour] || "Game folder: " + candidate.flavour));
    statusBox.append(candidate.sterling_version ? statusLine("good", "Sterling Quest Sync " + candidate.sterling_version + " installed") : statusLine("", "Sterling Quest Sync isn't installed here"));

    const accountCount = candidate.accounts.length;

    statusBox.append(accountCount ? statusLine("good", accountCount + (accountCount === 1 ? " WoW account found" : " WoW accounts found")) : statusLine("", "No WoW accounts yet. That's fine, log in once later."));
    $("cand-install-row").hidden = !!candidate.sterling_version;
    $("game-warn").hidden = forever;
    $("folder-go").disabled = false;
}

// Looks for WoW folders and lists them.
async function loadGames()
{
    gamesLoaded = true;

    try
    {
        found = await call("discover_games");
    }

    catch (error)
    {
        fail(error);
        found = [];
    }

    if (!found.length)
    {
        $("cand-path").textContent = "Couldn't find WoW on its own. Click Change to pick the folder.";
        $("folder-go").disabled = true;
    }

    else
    {
        showCandidate(await call("candidate_found", { index: 0 }));
    }

    const others = found.length - 1;

    $("cand-others").hidden = others < 1;
    $("cand-others").replaceChildren();

    if (others > 0)
    {
        const link = el("a", "", "Use one of those");

        link.href = "#";

        link.onclick = (event) =>
        {
            event.preventDefault();
            $("cand-list").hidden = !$("cand-list").hidden;
        };

        $("cand-others").append("Found " + others + " other WoW " + (others === 1 ? "version" : "versions") + " on this PC. ", link);
    }

    const list = $("cand-list");

    list.replaceChildren();

    found.forEach((candidate, index) =>
    {
        const button = el("button");

        button.type = "button";

        button.append(el("span", "", candidate.path), el("span", "pill", (FLAVOURS[candidate.flavour] || candidate.flavour) + (candidate.sterling_version ? " · Sterling" : "")));
        button.onclick = () => busy(null, async () =>
        {
            showCandidate(await call("candidate_found", { index: index }));
            list.hidden = true;
        });

        list.append(button);
    });
}

// Redraws the page that's open.
function renderApp()
{
    for (const button of document.querySelectorAll(".side > button"))
    {
        if (button.dataset.page === page) button.setAttribute("aria-current", "page"); else button.removeAttribute("aria-current");
    }

    for (const section of document.querySelectorAll(".page")) section.hidden = section.id !== "page-" + page;

    const srv = $("server");
    const dot = srv.querySelector(".dot");

    dot.className = "dot" + (state.online === false ? " bad" : state.online ? " good" : "");
    $("server-text").textContent = state.online === false ? "Server offline" : state.online ? "Server online" : "Checking server";

    const sel = $("char");
    const character = currentChar();

    sel.replaceChildren();

    if (!state.characters.length)
    {
        const option = el("option", "", "No characters yet");

        sel.append(option);
        sel.disabled = true;
    }

    else
    {
        sel.disabled = false;

        if (!character)
        {
            const option = el("option", "", "Select a character");

            option.value = "";
            option.disabled = true;
            option.selected = true;
            sel.append(option);
        }

        for (const listed of state.characters)
        {
            const option = el("option", "", listed.name);

            option.value = listed.guid;

            if (character && listed.guid === character.guid) option.selected = true;

            sel.append(option);
        }
        }

    $("thispc").textContent = "This PC: " + state.pc_label;

    for (const nameSpot of document.querySelectorAll(".charname")) nameSpot.textContent = character ? character.name : "your character";

    renderHome(character);

    if (page === "partners") renderPartners(character);
    if (page === "pair") renderPair(character);
    if (page === "activity") renderActivity();
    if (page === "settings") renderSettings();
}

// The numbered next steps on the home page.
function steps(list, head)
{
    const stepsList = $("h-steps"), stepsHead = $("h-steps-head");

    stepsList.replaceChildren(...(list || []).map(step => el("li", "", step)));
    stepsList.hidden = !list;
    stepsHead.textContent = head || "";
    stepsHead.hidden = !head;
}

// The home page: sync state, partners, updates and recent activity.
function renderHome(character)
{
    $("auto").checked = state.auto_sync;
    $("sync").disabled = state.busy || !character;
    $("sync").textContent = state.busy ? "Syncing..." : "Sync now";

    const dot = $("h-dot");

    steps(null);

    // No character chosen: Home shows only what to do next.
    const needSterling = !!state.game && !state.sterling_version;

    $("home-rest").hidden = !character && !needSterling;
    document.querySelector("#home-rest .grid").hidden = !character;
    $("hero-act").hidden = !character;
    $("h-last").hidden = !character;

    if (!character && state.characters.length)
    {
        $("h-char").textContent = "WELCOME";
        $("h-title").textContent = "Select a character to start";
        dot.className = "dot gold";
        $("h-line").textContent = "Pick one in the Character list at the bottom left.";
    }

    else if (!character)
    {
        $("h-char").textContent = "WELCOME";
        dot.className = "dot";

        if (!state.companion_installed)
        {
            $("h-title").textContent = "Helper add-on missing";
            $("h-line").textContent = "The Companion add-on (Sterling Quest Sync Companion) isn't in your AddOns folder. In Settings, pick your game folder again to put it back, then restart WoW.";
        }

        else if (!state.sterling_version)
        {
            $("h-title").textContent = "Sterling Quest Sync isn't installed";
            $("h-line").textContent = "This app works with the Sterling Quest Sync add-on. Click Install below, then restart WoW and log in.";
        }

        else if (state.helper_off)
        {
            $("h-title").textContent = "WoW didn't load the Companion add-on";
            dot.className = "dot bad";
            $("h-line").textContent = "The game saved, but the Companion add-on (Sterling Quest Sync Companion) wasn't running, so nothing reached the app.";
            steps
            ([
                "Exit WoW completely. /reload won't load a new add-on.",
                "Start WoW. At character select, click AddOns and make sure Sterling Quest Sync Companion is ticked.",
                "Log in, then /reload or log out. Your character shows up here.",
            ], "To fix it:");
        }

        else if (state.restart_game && state.game_running)
        {
            $("h-title").textContent = "Restart WoW";
            dot.className = "dot gold";
            $("h-line").textContent = "One more step: WoW needs a restart to find the new add-on.";
            steps
            ([
                "Exit WoW completely. Logging out or /reload won't do it.",
                "Start WoW and log in to any character.",
                "That's it. Your character shows up here in a few seconds.",
            ]);
        }

        else
        {
            $("h-title").textContent = "Almost there";
            $("h-line").textContent = "Log in to WoW and your characters show up here.";
            steps
            ([
                "Exit WoW completely and start it again. New add-ons only load when the game starts.",
                "At character select, click AddOns and make sure Sterling Quest Sync and Sterling Quest Sync Companion are both ticked.",
                "Log in, then type /reload. Your character shows up when the game saves.",
                "Still nothing? In Settings, check the game folder is the WoW Forever folder you play from.",
            ], "Not showing up?");
        }
    }

    else
    {
        $("h-char").textContent = character.name.toUpperCase();

        if (state.busy)
        {
            $("h-title").textContent = "Syncing...";
            dot.className = "dot gold";
            $("h-line").textContent = "Talking to the server.";
        }

        else if (state.last_error)
        {
            $("h-title").textContent = "Couldn't sync";
            dot.className = "dot bad";
            $("h-line").textContent = state.last_error;
        }

        else if (character.ready)
        {
            $("h-title").textContent = "Ready to sync";
            dot.className = "dot gold";
            $("h-line").textContent = "New progress from " + ago(character.saved) + (state.auto_sync ? ". Sharing it on its own." : ". Click Sync now to share it.");
        }

        else
        {
            $("h-title").textContent = "Up to date";
            dot.className = "dot good";
            $("h-line").textContent = character.saved ? "Your partners have your latest progress (saved " + ago(character.saved) + ")." : "Play with Sterling Quest Sync on, then /reload or log out to share your progress.";
        }
    }

    $("h-last").textContent = state.last_sync ? "Last synced " + ago(state.last_sync) : "Not synced yet";

    renderNotices();

    const appCard = $("app-card");
    const available = state.app_update || (state.app_latest && newer(state.app_latest, state.version) ? state.app_latest : null);
    const updating = state.updating;

    progress("app", updating);

    if (updating)
    {
        appCard.hidden = false;
        $("app-text").replaceChildren(el("b", "", "Updating the Companion"), el("span", "muted", available ? " to " : ""), el("b", "good-t", available || ""));
        $("app-update").hidden = true;
    }

    else if (available)
    {
        appCard.hidden = false;
        $("app-text").replaceChildren(el("span", "muted", "App update: "), el("b", "", "Companion "), el("span", "muted", state.version + " to "), el("b", "good-t", available));
        $("app-update").hidden = false; // the install finds and checks the signed update itself
    }

    else appCard.hidden = true;

    // A failed update says why, and offers the installer from the releases page instead.
    const failed = !updating && available && state.update_failed;

    $("app-fail").hidden = !failed;
    $("app-download").hidden = !failed;
    $("app-update").textContent = failed ? "Try again" : "Update";

    if (failed) $("app-fail").replaceChildren(el("b", "bad-t", "The update didn't work. "), el("span", "muted", state.update_failed + ". You can download the new version and run it yourself; your settings and partners stay."));

    $("helper-card").hidden = !(character && state.helper_off);

    const card = $("sterling-card");
    const installed = state.sterling_version, latest = state.sterling_latest;

    if (!installed && state.game)
    {
        card.hidden = false;
        $("sterling-text").replaceChildren(el("b", "", "Sterling Quest Sync "), el("span", "muted", "isn't installed in this game folder"));
        $("sterling-update").textContent = "Install";
    }

    else if (installed && latest && newer(latest, installed))
    {
        card.hidden = false;

        const mgr = (state.sterling_managed_by || [])[0];

        $("sterling-text").replaceChildren(el("span", "muted", "Add-on update: "), el("b", "", "Sterling Quest Sync "), el("span", "muted", installed + " to "), el("b", "good-t", latest), mgr ? el("span", "muted", ". Ready in " + mgr + ".") : "");
        $("sterling-update").textContent = "Update";
        // WowUp or the CurseForge app looks after the add-on: point there instead of updating it here.
        $("sterling-update").hidden = !!mgr;
    }

    else card.hidden = true;

    if (card.hidden || !(installed && latest && newer(latest, installed))) $("sterling-update").hidden = false;

    // Same rows as the Partners page: favourites (up to 5), or the first few until some are starred.
    const partnersBox = $("h-partners");

    partnersBox.replaceChildren();

    const all = character ? partnerRows(character) : [];
    const favs = all.filter((row) => row.fav);

    if (!all.length)
    {
        partnersBox.append(el("div", "empty", character ? "No partners yet. Partners you add in Sterling Quest Sync show up here." : "Select a character to start."));

        if (character)
        {
            const button = el("button", "btn small", "Pair a friend");

            button.type = "button";
            button.onclick = () => go("pair");
            partnersBox.append(button);
        }
    }

    else
    {
        const box = el("div", "plist flat");

        for (const row of (favs.length ? favs : all.slice(0, 3))) box.append(prow(row.opts));

        partnersBox.append(box);

        const foot = el("div", "row spread small muted hfoot");

        foot.append(el("span", "", favs.length ? "" : "Star partners to keep them here."));

        const more = el("button", "btn small ghost", "All partners (" + all.length + ")");

        more.type = "button";
        more.onclick = () => go("partners");

        foot.append(more);
        partnersBox.append(foot);
    }

    const recentBox = $("h-recent");

    recentBox.replaceChildren();

    if (!state.activity.length) recentBox.append(el("div", "empty", "Nothing yet."));

    for (const entry of state.activity.slice(0, 5)) recentBox.append(eventRow(entry));
}

// One line of activity.
function eventRow(entry)
{
    const row = el("div", "ev");

    row.append(el("span", "t", clock(entry.t)), el("span", entry.tone === "bad" ? "bad-t" : "", entry.text));

    return row;
}

const CLASS_NAMES = { WARRIOR: "Warrior", PALADIN: "Paladin", HUNTER: "Hunter", ROGUE: "Rogue", PRIEST: "Priest", SHAMAN: "Shaman", MAGE: "Mage", WARLOCK: "Warlock", DRUID: "Druid", DEATHKNIGHT: "Death Knight", MONK: "Monk", DEMONHUNTER: "Demon Hunter", EVOKER: "Evoker" };
const classCls = (look) => (look && CLASS_NAMES[look.class] ? " c-" + look.class : "");

// One row in Sterling's in-game style: name, status line, right side, menu.
function prow(opts)
{
    const row = el("div", "prow" + (opts.you ? " you" : ""));

    if (opts.you) row.append(el("span", "ptag", "YOU"));

    const dim = !!opts.dim;
    const middle = el("div", "pm");
    // First name (Sterling's nickname) on top, character name under it, like the list in game.
    const nick = opts.look && opts.look.nick;

    middle.append(el("div", "pn" + (dim ? " dim" : classCls(opts.look)), nick || opts.name || "Sterling partner"));

    const line = el("div", "pl");
    const bits = [];

    if (opts.status) bits.push(el("span", opts.statusCls || "", opts.status));
    if (nick && opts.name && nick !== opts.name) bits.push(opts.name);
    if (opts.look && opts.look.level) bits.push("Lvl " + opts.look.level);

    for (const bit of opts.extra || []) if (bit) bits.push(bit);

    // Each piece stays whole; a narrow list wraps only between pieces.
    const piece = (bit, extra) =>
    {
        const wrap = el("span", "bit" + (extra || ""));

        wrap.append(bit);

        return wrap;
    };

    bits.forEach((bit, index) =>
    {
        if (index) line.append(" · ");

        line.append(piece(bit));
    });

    // In a narrow list (Home) the right column folds into this line; see .pl-time in the stylesheet.
    if (opts.right && opts.right[1] && !opts.you)
    {
        const timeSpan = el("span", "pl-time");

        timeSpan.append(" · ", piece(opts.right[1]));
        line.append(timeSpan);
    }

    middle.append(line);
    row.append(middle);

    const rowEl = el("div", "pr");

    if (opts.right)
    {
        if (opts.right[0]) rowEl.append(el("div", "s " + (opts.rightCls || ""), opts.right[0]));
        if (opts.right[1]) rowEl.append(opts.right[1]);
    }

    row.append(rowEl);

    if (opts.star)
    {
        const starred = opts.star.on;
        const starButton = el("button", "ico star" + (starred ? " on" : ""), starred ? "\u2605" : "\u2606");

        starButton.type = "button";

        starButton.title = starred ? "Remove from favourites" : "Add to favourites";
        starButton.setAttribute("aria-pressed", starred ? "true" : "false");
        starButton.onclick = () => busy(starButton, async () =>
        {
            const ok = await call("set_favorite", { partner: opts.star.guid, on: !starred });

            if (ok === false) toast("You can favourite up to 5 partners.", true);
        });

        row.append(starButton);
    }

    else if (opts.menu && !opts.you) row.append(el("span", "ico-gap"));

    if (opts.menu)
    {
        const moreButton = el("button", "ico", "⋯");

        moreButton.type = "button";
        moreButton.title = "More";
        moreButton.setAttribute("aria-label", "More for " + (opts.name || "partner"));

        moreButton.onclick = opts.menu;
        row.append(moreButton);
    }

    return row;
}

// Last time anything synced with them: a copy through the app, or a live sync in game.
function lastTime(...times)
{
    const latest = Math.max(0, ...times.filter(Boolean));

    return latest ? "synced " + ago(latest) : "not synced yet";
}

// Every partner row for a character: favourites first (in the order starred), then live pairs,
// Sterling partners not paired in the app yet, and ended pairs.
function partnerRows(character)
{
    const favs = character.favs || [];
    const star = (guid) => ({ guid, on: favs.includes(guid) });
    const rows = [];
    const unpaired = new Set(character.unpaired.map((entry) => entry.guid));
    const list = character.partners.filter((partner) => !hidden.has(partner.pairing_id) && !(!partner.active && unpaired.has(partner.guid)));

    for (const partner of list.filter((partner) => partner.active))
    {
        rows.push({ guid: partner.guid, opts:
        {
            name: partner.name, look: partner.look, star: star(partner.guid),
            status: partner.last_copy ? (partner.applied ? "Up to date" : state.game_running ? "New copy, /reload to see it" : "New copy, log in to see it") : "Paired. Waiting for their next save",
            statusCls: partner.last_copy ? (partner.applied ? "s-ok" : "s-wait") : "",
            right: [null, lastTime(partner.last_copy, partner.look && partner.look.seen)],
            menu: async () =>
            {
                const ok = await dialog("Stop syncing with " + ((partner.look && partner.look.nick) || partner.name) + "?", [para("Their copies stop coming in and yours stop going out. You can pair again later with a new code.", "muted")], [["Cancel", false, "ghost"], ["Stop syncing", true, "bad"]]);

                if (ok) busy(null, () => call("end_pairing", { id: partner.pairing_id }));
            },
        } });
    }

    for (const waiting of character.unpaired)
    {
        const [status, cls] = waiting.offered ? ["Pairs when you're both next online", "s-wait"]
            : waiting.app === "yes" ? ["Has the app, pairs when you're both next online", "s-wait"]
            : waiting.app === "no" ? ["Doesn't have the app yet", "s-bad"]
            : ["Not online with you yet", ""];

        rows.push({ guid: waiting.guid, opts:
        {
            name: waiting.name, look: waiting.look, status, statusCls: cls, star: star(waiting.guid), right: [null, lastTime(waiting.look && waiting.look.seen)],
            menu: async () =>
            {
                const who = (waiting.look && waiting.look.nick) || waiting.name || "Sterling partner";

                if (waiting.app === "no")
                {
                    const choice = await dialog(who, [para("You still sync live in game when you're both online. To sync when you play at different times, they need the Sterling Quest Sync Companion app. Once they have it, the apps pair on their own the next time you're both online.", "muted"), para("Copy an invite with the download link and the setup steps, then paste it to them.", "small muted")], [["Close", false, "ghost"], ["Copy invite", "invite", "primary"]]);

                    if (choice === "invite") copy(inviteText(), "Invite copied. Paste it to " + who + ".");

                    return;
                }

                const go2 = await dialog(who, [para("This usually pairs on its own when you're both online in game. You can also pair now with a code.", "muted")], [["Close", false, "ghost"], ["Pair with a code", true]]);

                if (go2) go("pair");
            },
        } });
    }

    for (const partner of list.filter((partner) => !partner.active))
    {
        const replaced = partner.ended_reason === "replaced";
        const why = replaced ? "Needs pairing again" : partner.ended_reason === "deleted" ? "They deleted their data" : "Stopped syncing";

        rows.push({ guid: partner.guid, opts:
        {
            name: partner.name, look: partner.look, dim: !replaced,
            status: why, statusCls: replaced ? "s-bad" : "",
            right: [null, partner.ended_at ? "ended " + ago(partner.ended_at) : ""],
            menu: async () =>
            {
                const btns = [["Close", null, "ghost"], ["Hide", "hide"]];

                if (replaced) btns.push(["Pair again", "pair"]);

                const choice = await dialog((partner.look && partner.look.nick) || partner.name, [para(replaced ? "They reinstalled or paired again from somewhere else. Pair again with a new code." : "This pairing has ended. Hide it from the list, or pair again any time with a new code.", "muted")], btns);

                if (choice === "pair") go("pair");
                if (choice === "hide")
                {
                    hidden.add(partner.pairing_id);

                    try
                    {
                        localStorage.setItem("hiddenPairs", JSON.stringify([...hidden]));
                    }

                    catch (_) {}
                    renderApp();
                }
            },
        } });
    }

    for (const row of rows) row.fav = !!(row.opts.star && row.opts.star.on);

    const rank = (row) => (row.fav ? favs.indexOf(row.guid) : 99);

    return rows.map((row, index) => [row, index]).sort((left, right) => rank(left[0]) - rank(right[0]) || left[1] - right[1]).map((pair) => pair[0]);
}

// The partners page.
function renderPartners(character)
{
    $("p-note").textContent = "Stars and removals you make here reach the game " + (state.game_running ? "when you /reload." : "next time you log in.") + " Ones you make in game show up here after the game saves. If you change the same partner in both places, removing wins.";
    $("p-sub").replaceChildren();

    const you = $("p-you");

    you.replaceChildren();

    const body = $("p-rows");

    body.replaceChildren();

    // Same count as Sterling's list in game: every current partner, paired in the app or not.
    $("p-count").textContent = character ? (character.partners.filter((partner) => partner.active).length + character.unpaired.length) + " / 10" : "";

    if (!character)
    {
        body.append(el("div", "empty", "Select a character to start."));
        return;
    }

    const linked = character.partners.filter((partner) => partner.active).length;
    const inSterling = character.unpaired.length + linked;

    $("p-sub").append(inSterling ? linked + " of " + inSterling + " add-on partners also sync through the app. Star up to 5 favourites to keep them on top, here and in game." : "No partners yet. Add partners in Sterling Quest Sync in game, or pair a friend here with a code.");

    const look = character.look || {};
    const race = [look.race, CLASS_NAMES[look.class]].filter(Boolean).join(" ");

    you.append(prow
    ({
        you: true, name: character.name, look,
        extra: [race, (character.sterling || state.sterling_version) ? "Sterling " + (character.sterling || state.sterling_version) : "", "App " + state.version],
        right: character.ready ? ["Ready to sync", "save waiting"] : ["Up to date", state.last_sync ? "synced " + ago(state.last_sync) : ""],
        rightCls: character.ready ? "s-wait" : "s-ok",
    }));

    const rows = partnerRows(character);

    if (!rows.length) body.append(el("div", "empty", "No partners yet. Partners you add in Sterling Quest Sync show up here, or pair a friend with a code."));

    for (const row of rows) body.append(prow(row.opts));
}

let codesFor = null, openCodes = [], latestId = null;

// Loads this character's open pair codes.
async function loadCodes(character)
{
    if (!character) return;

    try
    {
        openCodes = (await call("open_codes")).filter((code) => code.guid === character.guid);
    }

    catch (error)
    {
        openCodes = [];
    }

    codesFor = character.guid;
    drawCodes(character);
}

// Shows the newest pair code and the older ones.
function drawCodes(character)
{
    const latest = latestId && openCodes.find((code) => code.id === latestId);

    $("give-empty").hidden = !!latest;
    $("give-made").hidden = !latest;

    if (latest) $("latest-code").textContent = madeCodes[latest.id];

    const others = openCodes.filter((code) => !latest || code.id !== latest.id);

    $("older").hidden = !others.length;

    const olderList = $("older-list");

    olderList.replaceChildren();

    for (const code of others)
    {
        const row = el("div", "li");

        row.append(el("span", madeCodes[code.id] ? "mono" : "small muted", madeCodes[code.id] || "Code made earlier, ends " + clock(code.expires_at)));

        const cancelButton = el("button", "btn small ghost", "Cancel");

        cancelButton.type = "button";

        cancelButton.onclick = () => busy(cancelButton, async () =>
        {
            await call("cancel_code", { id: code.id });
            await loadCodes(character);
        });

        row.append(cancelButton);
        olderList.append(row);
    }

    $("pair-count").replaceChildren();
    $("pair-count").append(character.name + " has " + liveCount(character) + " of 10 partners. Need more than 10? ");

    const link = el("a", "", "Let us know");

    link.href = "#";
    link.onclick = (event) =>
    {
        event.preventDefault();
        call("open_page", { which: "issues" });
    };

    $("pair-count").append(link, ".");
}

// The pair page.
function renderPair(character)
{
    $("gen").disabled = !character;
    $("check-code").disabled = !character;

    if (!character)
    {
        $("pair-count").textContent = "Log in on a character first, then come back here.";
        return;
    }

    if (codesFor !== character.guid)
    {
        latestId = null;
        loadCodes(character);
    }

    else drawCodes(character);
}

// Makes a new pair code and shows it.
async function makeCode(btn)
{
    const character = currentChar();

    if (!character) return;

    await busy(btn, async () =>
    {
        const made = await call("make_pair_code", { guid: character.guid });

        madeCodes[made.id] = made.code;
        latestId = made.id;

        await loadCodes(character);
    });
}

// The activity page.
function renderActivity()
{
    const list = $("act-list");

    list.replaceChildren();

    if (!state.activity.length) list.append(el("div", "empty", "Nothing yet."));

    for (const entry of state.activity) list.append(eventRow(entry));
}

let pcsLoaded = false;

// Lists the PCs linked to this account.
async function loadPcs()
{
    pcsLoaded = true;

    const box = $("pcs");

    try
    {
        const account = await call("account");

        box.replaceChildren();

        for (const pc of account.pcs)
        {
            const row = el("div", "li");
            const left = el("span");

            left.append(el("span", "n", pc.label), pc.current ? el("span", "muted", " (this PC)") : "");

            if (pc.current) row.append(left, el("span", "small good-t", "Active now"));

            else
            {
                const right = el("span", "row");

                right.append(el("span", "small muted", "Last seen " + ago(pc.last_seen)));

                const removeButton = el("button", "btn small ghost", "Remove");

                removeButton.type = "button";

                removeButton.onclick = async () =>
                {
                    const ok = await dialog("Remove " + pc.label + "?", [para("That PC stops syncing until it's linked again. Nothing on it is deleted.", "muted")], [["Cancel", false, "ghost"], ["Remove", true, "bad"]]);

                    if (ok) busy(removeButton, async () =>
                    {
                        await call("remove_pc", { id: pc.id });

                        loadPcs();
                    });
                };

                right.append(removeButton);
                row.append(left, right);
            }

            box.append(row);
        }
    }

    catch (error)
    {
        box.replaceChildren(el("div", "small muted", "Couldn't load your PCs: " + error));
    }
}

// The settings page.
function renderSettings()
{
    $("s-auto").checked = state.auto_sync;
    $("s-sterling-auto").checked = state.auto_update_sterling;
    $("s-app-auto").checked = state.auto_update_app;
    $("s-appver").textContent = state.version;
    renderAppUpdateSettings();
    $("s-tray").checked = state.close_to_tray;
    $("s-startup").checked = state.start_with_windows;
    // Switched off outside the app (Task Manager, Windows Settings, another program): it stays off, and says so.
    $("s-startup-note").hidden = !state.autostart_note;
    $("s-startup-note").textContent = state.autostart_note || "";
    $("s-notify").checked = state.notify_favs;

    const mgrs = state.sterling_managed_by || [];

    $("s-sterling").textContent = !state.sterling_version ? "Sterling Quest Sync isn't installed in this game folder."
        : mgrs.length ? "You have " + state.sterling_version + ". " + mgrs.join(" or ") + " is on this PC, so updates are left to it and the Companion just tells you when one is out."
        : "You have " + state.sterling_version + ". Updates install for you, and the old version is kept as a backup.";
    $("s-path").textContent = state.game_path || "Not set";

    if (!pcsLoaded) loadPcs();
}

// Opens a page.
function go(target)
{
    page = target;

    if (target === "settings") pcsLoaded = false;

    renderApp();
}

// ---------------------------------------------------------------- wiring

$("win-min").onclick = () => tauri.window.getCurrentWindow().minimize().catch(fail);
$("win-close").onclick = async () =>
{
    if (state && state.close_to_tray && !state.close_note_seen && !state.setup_needed)
    {
        const choice = await dialog("Still running in the tray", [para("The Companion keeps syncing in the background. Find it by the clock, and quit any time from its tray icon.", "muted"), para("You can change what the close button does in Settings.", "small muted")], [["Quit instead", "quit", "ghost"], ["Got it", "tray", "primary"]]);

        if (!choice) return;

        await call("set_options", { closeNoteSeen: true }).catch(() => {});

        if (choice === "quit") return call("quit");
    }

    call("close_window");
};

for (const button of document.querySelectorAll("[data-page]")) button.addEventListener("click", () => go(button.dataset.page));
for (const button of document.querySelectorAll("[data-copy]")) button.addEventListener("click", () => copy($(button.dataset.copy).textContent));
for (const link of document.querySelectorAll("[data-open]")) link.addEventListener("click", (event) =>
{
    event.preventDefault();
    call("open_page", { which: link.dataset.open });
});

for (const button of document.querySelectorAll("[data-back]")) button.addEventListener("click", () =>
{
    if (button.dataset.back === "folder")
    {
        gamesLoaded = false;
        showStep("folder");
        loadGames();
        return;
    }

    setupStep = "first";

    if (state) render(state);
});

for (const radio of document.querySelectorAll("input[name=first]")) radio.addEventListener("change", () =>
{
    for (const choice of document.querySelectorAll("#step-first .choice")) choice.classList.toggle("framed", choice.querySelector("input").checked);
});

const OTHER_WOW = ["_classic_era_", "_classic_", "_anniversary_", "_retail_", "_ptr_", "_beta_", "_classic_ptr_", "_xptr_"];

// Warns when the picked folder isn't WoW Forever.
async function warnIfNotForever(candidate)
{
    if (candidate && OTHER_WOW.includes(candidate.flavour))
    {
        await dialog("Made for WoW Forever", [para("You picked " + (FLAVOURS[candidate.flavour] || candidate.flavour) + ". Sterling Quest Sync and the Companion are built and tested for WoW Forever. With another version of WoW they may not work as expected.", "muted"), para("You can switch back any time in Settings, under Game folder.", "small muted")], [["OK", true, "primary"]]);
    }
}

$("cand-change").onclick = () => busy($("cand-change"), async () =>
{
    const candidate = await call("candidate_pick");

    if (candidate) showCandidate(candidate);
});

$("folder-go").onclick = () => busy($("folder-go"), async () =>
{
    const install = !$("cand-install-row").hidden && $("cand-install").checked;

    if (install) $("folder-go").textContent = "Installing Sterling Quest Sync...";

    try
    {
        await call("confirm_game", { installSterling: install });
    }

    finally
    {
        $("folder-go").textContent = "Continue";
    }

    if (state.game_running) toast("WoW is open. Exit it completely and start it again so it finds the new add-on.");

    setupStep = "first";
});

$("to-recover").onclick = (event) =>
{
    event.preventDefault();
    setupStep = "recover";
    render(state);
};

$("first-go").onclick = () =>
{
    const yes = document.querySelector("input[name=first]:checked").value === "yes";

    if (!yes)
    {
        $("pcname2").value = $("pcname1").value;
        setupStep = "link";

        return render(state);
    }

    busy($("first-go"), async () =>
    {
        const joined = await call("create_account", { label: $("pcname1").value, invite: null });

        pendingRec = joined.recovery_code;
        render(await call("status"));
    });
};

$("link-go").onclick = () => busy($("link-go"), async () =>
{
    await call("link_this_pc", { code: $("linkcode").value, label: $("pcname2").value });

    setupStep = "first";
    render(await call("status"));
});

$("recover-go").onclick = () => busy($("recover-go"), async () =>
{
    const joined = await call("recover_account", { code: $("reccode-in").value, label: $("pcname3").value });

    pendingRec = joined.recovery_code;
    setupStep = "first";
    render(await call("status"));
});

$("rec-txt").onclick = () => busy($("rec-txt"), async () =>
{
    if (await call("save_recovery_txt", { code: pendingRec })) toast("Saved");
});

const recDone = () =>
{
    pendingRec = null;
    $("reccode").textContent = "";
    page = "home";
    render(state);
};

$("rec-skip").onclick = recDone;
$("rec-done").onclick = recDone;
$("rj-fresh").onclick = () => busy($("rj-fresh"), async () =>
{
    const joined = await call("create_account", { label: state.pc_label, invite: null });

    pendingRec = joined.recovery_code;
    render(await call("status"));
});

$("rj-link").onclick = () =>
{
    setupStep = "link";
    render(state);
};

$("rj-recover").onclick = () =>
{
    setupStep = "recover";
    render(state);
};

$("sync").onclick = () => call("sync_now").catch(fail);
$("auto").onchange = (event) => call("set_options", { autoSync: event.target.checked }).catch(fail);
$("s-auto").onchange = (event) => call("set_options", { autoSync: event.target.checked }).catch(fail);
$("s-tray").onchange = (event) => call("set_options", { closeToTray: event.target.checked }).catch(fail);
$("s-startup").onchange = (event) => call("set_options", { startWithWindows: event.target.checked }).catch((err) =>
{
    event.target.checked = !event.target.checked;
    fail(err);
});

$("s-notify").onchange = (event) => call("set_options", { notifyFavs: event.target.checked }).catch(fail);
$("s-details").onclick = () => busy($("s-details"), async () => copy(await call("support_details"), "Details copied. Paste them to whoever is helping you."));
$("s-app-auto").onchange = (event) => call("set_options", { autoUpdateApp: event.target.checked }).catch(fail);
// A newer app version, from the updater's own check or from the server's note of the latest version.
// The owner's notices from the server, already checked and picked for this version by the app.
// Text is only ever set with textContent; the button opens the link through the app, which checks it again.
function renderNotices()
{
    const box = $("notices");
    const list = state.notices || [];

    box.hidden = !list.length;
    box.replaceChildren(...list.map((notice) =>
    {
        const card = el("div", "microcard notice " + (["info", "warn", "bad"].includes(notice.level) ? notice.level : "info"));

        card.setAttribute("role", notice.level === "bad" ? "alert" : "status");
        card.append(el("span", "notice-t", notice.text));

        const acts = el("span", "notice-acts");

        if (notice.link)
        {
            const openButton = el("button", "btn small", "Open");

            openButton.type = "button";

            openButton.onclick = () => call("open_notice", { id: notice.id }).catch(fail);
            acts.append(openButton);
        }

        const closeButton = el("button", "btn small ghost notice-x", "\u00d7");

        closeButton.type = "button";

        closeButton.title = "Hide this message";
        closeButton.setAttribute("aria-label", "Hide this message");
        closeButton.onclick = () => call("dismiss_notice", { id: notice.id }).catch(fail);
        acts.append(closeButton);
        card.append(acts);

        return card;
    }));
}

// The newer app version available, or null.
function appAvail()
{
    return state.app_update || (state.app_latest && newer(state.app_latest, state.version) ? state.app_latest : null);
}

// Shared update progress: a step line and a bar (filled by percent, or a moving sliver when there's no number).
function progress(prefix, update)
{
    $(prefix + "-progress").hidden = !update;

    if (!update) return;

    $(prefix + "-step").textContent = update.percent != null ? update.step + ": " + update.percent + "%" : update.step;

    const fill = $(prefix + "-fill");

    fill.parentElement.classList.toggle("wait", update.percent == null);
    fill.style.width = update.percent != null ? update.percent + "%" : "";
}

let lastCheck = null; // { found, at } from the Settings button

// The app update part of settings.
function renderAppUpdateSettings()
{
    const updating = state.updating, ready = appAvail();

    progress("s", updating);
    $("s-check").hidden = !!updating;
    $("s-update").hidden = !!updating || !ready;

    if (ready) $("s-update").textContent = "Update to " + ready;

    const note = $("s-ver-note");

    note.className = "small muted block";

    if (updating) note.textContent = "Updating" + (ready ? " to " + ready : "") + ". The app reopens when it's done.";

    else if (ready)
    {
        note.textContent = "Version " + ready + " is ready to install.";
        note.className = "small good-t block";
    }

    else if (lastCheck && lastCheck.err)
    {
        note.textContent = "Couldn't check just now. Try again in a bit.";
        note.className = "small bad-t block";
    }

    else if (lastCheck) note.textContent = "You have the newest version. Checked " + ago(lastCheck.at) + ".";
    else note.textContent = "Checks on its own every few hours.";
}

$("s-check").onclick = () => busy($("s-check"), async () =>
{
    $("s-check").textContent = "Checking...";
    $("s-ver-note").textContent = "Checking for updates...";

    try
    {
        const newVersion = await call("check_for_app_update");

        lastCheck = { found: newVersion, at: Math.floor(Date.now() / 1000) };
    }

    catch (error)
    {
        lastCheck = { err: true, at: Math.floor(Date.now() / 1000) };
        throw error;
    }

    finally
    {
        $("s-check").textContent = "Check for updates";

        if (state) renderAppUpdateSettings();
    }
});

// Asks before updating the app, then updates.
async function askUpdate(btn)
{
    const ok = await dialog("Update the Companion?", [para("The app closes for a few seconds, installs version " + appAvail() + " and opens again. Your settings and partners stay.", "muted")], [["Not now", false, "ghost"], ["Update", true, "primary"]]);

    if (ok) busy(btn, () => call("update_app"));
}

$("app-update").onclick = () => askUpdate($("app-update"));
$("app-download").onclick = () => call("open_page", { which: "download" }).catch(fail);
$("s-update").onclick = () => askUpdate($("s-update"));
$("s-sterling-auto").onchange = (event) => call("set_options", { autoUpdateSterling: event.target.checked }).catch(fail);
$("char").onchange = (event) => call("set_options", { character: event.target.value }).catch(fail);
$("sterling-update").onclick = () => busy($("sterling-update"), async () =>
{
    const result = await call("update_sterling");

    toast("Sterling Quest Sync " + result + " installed" + (state.game_running ? (state.restart_game ? ". Exit WoW completely and start it again to load it." : ". Type /reload in game to load it.") : ""));
});

$("gen").onclick = () => makeCode($("gen"));
$("gen2").onclick = () => makeCode($("gen2"));
$("cancel-latest").onclick = () => busy($("cancel-latest"), async () =>
{
    const character = currentChar();

    await call("cancel_code", { id: latestId });

    latestId = null;

    await loadCodes(character);
});

$("check-code").onclick = () => busy($("check-code"), async () =>
{
    const character = currentChar();
    const code = $("paircode").value.trim();

    if (!code) throw "Type the code your friend sent.";

    const peek = await call("peek_code", { guid: character.guid, code });

    if (peek.already_paired)
    {
        await dialog("Already paired", [para("You already sync " + character.name + " with " + peek.partner.name + ". Nothing to do.")], [["OK", true, "primary"]]);

        $("paircode").value = "";
        return;
    }

    const body = [facts([["Their character", peek.partner.name], ["Realm", peek.partner.realm], ["Your character", character.name]])];

    if (peek.different_install) body.push(warnBox("You already sync with " + peek.partner.name, "This code comes from a different install. Check with them before accepting. If they reinstalled, this is expected."));

    body.push(para(peek.different_install ? "Accepting replaces the old pairing, sets new keys, and sends a full copy both ways." : "You'll each get the other's latest copy after your next save.", "small muted"));

    const ok = await dialog("Pair with " + peek.partner.name + "?", body, [["Cancel", false, "ghost"], [peek.different_install ? "Replace and pair" : "Pair", true, "primary"]]);

    if (!ok) return;

    await call("redeem_code", { guid: character.guid, code, replace: !!peek.different_install });

    $("paircode").value = "";
    toast("Paired with " + peek.partner.name);
    go("partners");
});

$("link-pc").onclick = () => busy($("link-pc"), async () =>
{
    const link = await call("link_code");

    await dialog("Link another PC", [para("On the other PC, install Sterling Quest Sync Companion and choose \"No, I already use it on another PC\". Type this code there.", "muted"), codeBox(link.code), para("Lasts 10 minutes and works once.", "small muted")], [["Done", true, "primary"]]);

    pcsLoaded = false;
    renderApp();
});

$("new-rec").onclick = async () =>
{
    const ok = await dialog("Make a new recovery code?", [para("Your old recovery code stops working right away.", "muted")], [["Cancel", false, "ghost"], ["Make a new code", true, "primary"]]);

    if (!ok) return;

    busy($("new-rec"), async () =>
    {
        const code = await call("new_recovery_code");
        const download = el("button", "btn", "Download .txt");

        download.type = "button";

        download.onclick = () => call("save_recovery_txt", { code }).then((saved) => saved && toast("Saved"), fail);

        await dialog("Your new recovery code", [para("Keep it somewhere safe, like a password manager. Shown once.", "muted"), codeBox(code), download], [["I saved it", true, "primary"]]);
    });
};

$("s-change").onclick = () => busy($("s-change"), async () => warnIfNotForever(await call("pick_game_folder")));
$("s-backups").onclick = () => call("open_backups").catch(fail);
$("s-delete").onclick = async () =>
{
    const ok = await dialog("Delete your data?", [para("This removes your account, every PC, your pairings and your copies from the server, right away. Copies already on friends' PCs stay there. Your quests in game are not touched.", "muted"), warnBox("This can't be undone", "To sync again you'd start a new account and pair with friends again.")], [["Cancel", false, "ghost"], ["Delete everything", true, "bad"]]);

    if (!ok) return;

    busy($("s-delete"), async () =>
    {
        await call("delete_my_data");

        toast("Your data was deleted from the server");
    });
};

tauri.event.listen("status", (event) => render(event.payload));
call("status").then(render, fail);
