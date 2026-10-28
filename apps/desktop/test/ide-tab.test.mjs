import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-ide.ts");
const view = await import("../src/screens/project/ide.tsx");
const page = await import("../src/screens/project/index.tsx");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const state = (build, unqualified = null) => ({ build, unqualified });
const shown = (ideState) => renderToStaticMarkup(createElement(view.IdeStatus, { state: ideState, busy: false, onAction: () => {} }));
const escaped = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("\"", "&quot;").replaceAll("'", "&#x27;");

test("each build state renders its plain line, and only Missing and Failed offer an action", () => {
  const cases = [
    [{ state: "missing" }, "The IDE isn't downloaded yet.", "download"],
    [{ state: "downloading", bytes: 48 * 1024 * 1024, total: 96 * 1024 * 1024 }, "Downloading the IDE: 48 of 96 MB.", null],
    [{ state: "verifying" }, "Checking the IDE download.", null],
    [{ state: "ready", version: "1.143.17183", root: "/data/ide/build" }, "Opening the IDE.", null],
    [{ state: "failed", code: "ide_build_hash_mismatch" }, "The IDE download didn't match its published checksum, so it was deleted and never run.", "retry"],
  ];
  for (const [build, line, action] of cases) {
    const status = client.ideStatus(state(build));
    assert.equal(status.line, line, build.state);
    assert.equal(status.action, action, build.state);
    assert.equal(status.ready, build.state === "ready", build.state);
    const markup = shown(state(build));
    assert.ok(markup.includes(escaped(line)), `${build.state}: ${markup}`);
    assert.equal(markup.includes(">Retry<"), action === "retry", `${build.state} Retry: ${markup}`);
    assert.equal(markup.includes(">Download<"), action === "download", `${build.state} Download: ${markup}`);
  }
  const unknown = client.ideStatus(state({ state: "failed", code: "ide_made_up" }));
  assert.equal(unknown.action, "retry");
  assert.ok(unknown.line.endsWith("(ide_made_up)"), unknown.line);
  assert.equal(client.ideStatus(state({ state: "downloading", bytes: 0, total: 0 })).line, "Downloading the IDE.");
});

test("an unqualified platform shows Unavailable with the platform name and offers nothing", () => {
  for (const [platform, name] of [["linux", "Linux"], ["windows", "Windows"], ["freebsd", "freebsd"]]) {
    const status = client.ideStatus(state({ state: "ready", version: "1.143.17183", root: "/x" }, platform));
    assert.equal(status.line, `The IDE is unavailable on ${name}.`);
    assert.equal(status.action, null);
    assert.equal(status.ready, false, "a ready build never opens where the server cannot run");
    const markup = shown(state({ state: "missing" }, platform));
    assert.ok(markup.includes(`The IDE is unavailable on ${name}.`), markup);
    assert.ok(!markup.includes("<button"), markup);
  }
  assert.equal(client.openFailure("ide_platform_unqualified"), "The IDE is unavailable on this platform.");
  assert.equal(client.openFailure("ide_build_not_ready"), "The IDE isn't ready yet. Wait for the download to finish.");
  for (const code of ["ide_extension_install_failed", "ide_extension_list_invalid", "ide_extension_hash_mismatch", "ide_extensions_unavailable"]) {
    assert.equal(client.openFailure(code), "The IDE's extensions couldn't be installed, so the IDE didn't start. Try again.", code);
  }
  for (const code of ["ide_capability_capacity", "ide_capability_unavailable"]) {
    assert.equal(client.openFailure(code), "The IDE couldn't get its private link to CrazyAgent, so it didn't start. Try again.", code);
  }
});

test("the tab is absent when ide_tab is off and last when it is on", () => {
  const ids = (tabs) => tabs.map((tab) => tab.id);
  const base = ids(page.TABS);
  assert.ok(!base.includes("ide"));
  assert.deepEqual(ids(page.projectTabs(undefined)), base, "while the flag is unread, no IDE tab");
  assert.deepEqual(ids(page.projectTabs({ tab: "off" })), base);
  assert.deepEqual(ids(page.projectTabs({ tab: "on", state: state({ state: "missing" }) })), [...base, "ide"]);
  assert.equal(page.projectTabs({ tab: "on", state: state({ state: "missing" }) }).at(-1).label, "IDE");
  const lib = read("src-tauri/src/bridge/ide.rs");
  assert.match(lib, /IDE_TAB_FLAG: &str = "CRAZYAGENT_IDE_TAB"/, "the flag is the desktop's own switch until IDE-15");
  assert.match(lib, /return Ok\(Box::pin\(async \{ Ok\(IdeView::Off\) \}\)\);/, "with the flag off the engine is never asked");
});

test("the app menu the IDE tab uses owns none of Cmd+Z, W, N, P, F or comma", () => {
  const source = read("src-tauri/src/ide.rs");
  const start = source.indexOf("fn ide_menu");
  const menu = source.slice(start, source.indexOf("\n}\n", start));
  assert.ok(start >= 0 && menu.includes("PredefinedMenuItem::copy"), "the IDE menu keeps the clipboard items");
  for (const owned of ["undo", "redo", "close_window", "MenuItem::with_id", "MenuItemBuilder", "accelerator", "CmdOrCtrl", "Cmd+"]) {
    assert.ok(!menu.includes(owned), `the IDE menu holds ${owned}`);
  }
  assert.match(source, /ide_menu\(app\)\.and_then\(\|menu\| app\.set_menu\(menu\)\)/, "showing the IDE installs its menu");
  const show = source.slice(source.indexOf("pub fn show("), source.indexOf("pub fn place("));
  assert.ok(show.indexOf("ide_menu(app)") < show.indexOf("set_focus()"), "the menu is swapped before the IDE takes focus");
  assert.match(source, /set_menu\(Menu::default\(app\)/, "hiding it gives the app its own menu back");
});

test("the IDE webview runs the input guard, locks navigation and opens other links outside", () => {
  const source = read("src-tauri/src/ide.rs");
  assert.match(source, /include_str!\("ide_guard\.js"\)/);
  assert.match(source, /\.initialization_script_for_all_frames\(GUARD\)/, "extension webview frames are guarded too");
  assert.match(source, /\.on_navigation\(move \|next\| navigation_allowed\(next, &server, port\)\)/, "a refused load is only refused");
  assert.doesNotMatch(source, /on_navigation\([^\n]*outside/, "no load, frame or not, opens the browser");
  assert.match(source, /\.on_new_window\(/);
  assert.match(source, /NewWindowResponse::Deny/);
  assert.match(source, /open::open_link/);
  assert.match(source, /link_admitted\(&LINKS, Instant::now\(\)\)/, "links from window.open reach the browser at most once a second");
  const allowed = JSON.parse(read("src-tauri/capabilities/main-window.json"));
  assert.deepEqual(allowed.webviews, ["main"], "the IDE webview gets no app command");
});

test("the IDE tab follows its panel, hides with the tab, closes with the project and beats every minute", () => {
  const source = read("src/screens/project/ide.tsx");
  assert.equal(client.HEARTBEAT_MS, 60_000);
  assert.match(source, /window\.requestAnimationFrame\(follow\)/);
  assert.match(source, /hideIde\(/);
  assert.match(source, /touchIde\(/);
  const index = read("src/screens/project/index.tsx");
  assert.match(index, /closeIde\(projectId\)/, "leaving the project closes its IDE webview");
  assert.deepEqual(client.bounds({ left: 10.4, top: 80.6, width: 900.2, height: 600.7 }), { x: 10, y: 81, width: 900, height: 601 });
  assert.equal(client.bounds({ left: 0, top: 0, width: 0, height: 300 }), null, "a hidden panel has no bounds");
});

test("an open dialog, drawer or menu hides the IDE webview until the last one closes", async () => {
  const overlay = await import("../src/ui/overlay.tsx");
  assert.equal(overlay.overlayShown(), false);
  const first = overlay.cover();
  const second = overlay.cover();
  assert.equal(overlay.overlayShown(), true);
  first();
  first();
  assert.equal(overlay.overlayShown(), true, "a release counts once");
  second();
  assert.equal(overlay.overlayShown(), false);
  const ui = read("src/ui/overlay.tsx");
  const modal = ui.slice(ui.indexOf("function useModal"), ui.indexOf("export function Dialog"));
  assert.match(modal, /useCover\(open\)/, "every Dialog, Drawer and ConfirmDialog counts while open");
  const menu = ui.slice(ui.indexOf("export function Menu"));
  assert.match(menu, /useCover\(open\)/, "an open menu counts too");
  const tab = read("src/screens/project/ide.tsx");
  assert.match(tab, /const covered = useOverlayShown\(\);/);
  assert.match(tab, /if \(!ready \|\| !element \|\| covered\) return;/, "while covered the webview is hidden and not opened");
  assert.match(tab, /\}, \[projectId, ready, covered\]\);/, "it opens again once the overlay closes");
});

test("an open the tab gave up on is refused quietly instead of showing a problem", () => {
  const tab = read("src/screens/project/ide.tsx");
  assert.match(tab, /result\.code === "ide_open_cancelled"/);
  const lib = read("src-tauri/src/lib.rs");
  const open = lib.slice(lib.indexOf("async fn desktop_ide_open"), lib.indexOf("async fn desktop_ide_place"));
  assert.ok(open.indexOf("views.begin(&project)") >= 0 && open.indexOf("views.begin(&project)") < open.indexOf("ide_open(project"), "the ticket is taken before the engine call");
  assert.match(open, /ide::show\(&app, &window, &views, &project, workbench, bounds, ticket\)/);
});

test("the TUI focus poll reaches the engine only while a project's IDE webview is loaded", () => {
  const lib = read("src-tauri/src/lib.rs");
  const start = lib.indexOf("async fn desktop_ide_focus");
  const focus = lib.slice(start, lib.indexOf("#[tauri::command]", start));
  assert.ok(start >= 0, "the focus command exists");
  assert.match(focus, /views: State<'_, ide::IdeViews>/);
  assert.match(focus, /if !views\.any_loaded\(\) \{\s*return Ok\(None\);\s*\}/, "with no IDE loaded no reveal can mark a focus");
  assert.ok(focus.indexOf("views.any_loaded()") < focus.indexOf("ide_focus()"), "the engine is not asked, and its bridge lock not taken, before the check");
  assert.ok(focus.indexOf("main_window(&window)?") < focus.indexOf("views.any_loaded()"), "only the main window may ask");
});

const command = (lib, name) => {
  const start = lib.indexOf(`async fn ${name}(`);
  assert.ok(start >= 0, `${name} exists`);
  return lib.slice(start, lib.indexOf("#[tauri::command]", start));
};

test("the IDE webview follows its panel through any layout shift or scroll, and the tab fills the space left", () => {
  const box = { x: 0, y: 120, width: 900, height: 600 };
  assert.equal(client.sameBounds(box, { ...box }), true);
  assert.equal(client.sameBounds(box, null), false, "the first measure always places");
  for (const moved of [{ ...box, y: 168 }, { ...box, x: 1 }, { ...box, width: 899 }, { ...box, height: 640 }]) {
    assert.equal(client.sameBounds(box, moved), false, JSON.stringify(moved));
  }
  const tab = read("src/screens/project/ide.tsx");
  assert.match(tab, /window\.requestAnimationFrame\(follow\)/, "the panel's box is compared every frame while shown, so a banner or a scroll that moves it without a resize moves the IDE too");
  assert.match(tab, /window\.cancelAnimationFrame\(frame\)/, "hiding the tab stops following");
  assert.match(tab, /if \(!box \|\| sameBounds\(box, last\)\) return;/, "the window hears only a box that changed");
  assert.match(tab, /const place = \(\) => \{\s*if \(!shown\) return;\s*const box = bounds\(element\.getBoundingClientRect\(\)\);/, "nothing is measured before the IDE shows");
  assert.match(tab, /if \(!live \|\| !shown\) \{\s*frame = 0;\s*return;\s*\}/, "the loop runs only while the IDE is shown");
  assert.match(tab, /if \(frame === 0\) follow\(\);/, "the loop starts once the IDE shows, never twice");
  const css = read("src/screens/project/ide.css");
  assert.doesNotMatch(css, /100vh/, "the tab is not sized from a guessed header height");
  assert.match(css, /\.main:has\(\.ide-tab\)/, "the page lays out as a column while the IDE tab is shown");
  assert.match(css, /\.ide-tab \{[^}]*flex: 1;/, "the tab takes the space left under the header, banner and tabs");
});

test("closing a project page stops its IDE server, ordered with any open of the same project", () => {
  const lib = read("src-tauri/src/lib.rs");
  const close = command(lib, "desktop_ide_close");
  assert.ok(close.indexOf("views.closing(&project)") >= 0 && close.indexOf("views.closing(&project)") < close.indexOf("ide::close("), "the close is noted before the webview closes and before any await");
  assert.match(close, /views\s*\.stop_closed\(&project, closing,/);
  assert.match(close, /ide_stop\(project\.clone\(\)\)/, "the engine stops the server");
  const open = command(lib, "desktop_ide_open");
  assert.match(open, /let \(ticket, turn\) = views\.begin\(&project\);/, "an open takes its ticket and its turn under one lock");
  assert.doesNotMatch(open, /views\.turn\(/, "no second lock where a close could remove the order between them");
  assert.ok(open.indexOf("turn.lock().await") < open.indexOf("ide_open(project"), "the engine opens only in the project's turn");
  const bridge = read("src-tauri/src/bridge/ide.rs");
  assert.match(bridge, /client\.ide_tab_stop\(project\)/);
  const engine = read("../../crates/crazyagent/src/engine/owner/transport/ide_tab.rs");
  assert.match(engine, /Action::Stop \{ project \} => Ok\(Reply::Stopped \{/);
  assert.match(engine, /authority\.ide_servers\.stop\(project\)\.await/);
});

test("a loaded IDE hears the heartbeat on every tab of its project page, and only a loaded one reaches the engine", () => {
  const index = read("src/screens/project/index.tsx");
  assert.match(index, /window\.setInterval\(\(\) => void touchIde\(projectId\), HEARTBEAT_MS\)/, "the page beats while open, not only while the IDE is shown");
  assert.match(index, /if \(ide\?\.tab !== "on"\) return;/, "no beat with the IDE tab off");
  const touch = command(read("src-tauri/src/lib.rs"), "desktop_ide_touch");
  assert.match(touch, /if !views\.loaded\(&project\) \{\s*return Ok\(false\);\s*\}/);
  assert.ok(touch.indexOf("views.loaded(&project)") < touch.indexOf("ide_touch(project)"), "a project with no IDE loaded never asks the engine");
});

test("the IDE ask polls run only with the IDE tab on, and reach the engine only while an IDE is loaded", () => {
  const index = read("src/screens/project/index.tsx");
  assert.match(index, /\{ide\?\.tab === "on" && <IdeConfirm data=\{data\} \/>\}/);
  assert.match(index, /\{ide\?\.tab === "on" && <IdeExtensionConfirm data=\{data\} \/>\}/);
  const lib = read("src-tauri/src/lib.rs");
  for (const [name, engine] of [["desktop_ide_asks", "ide_asks(&id)"], ["desktop_ide_extension_asks", "ide_extension_asks(project_id)"]]) {
    const body = command(lib, name);
    assert.match(body, /if !views\.any_loaded\(\) \{\s*return Ok\(Vec::new\(\)\);\s*\}/, name);
    assert.ok(body.indexOf("views.any_loaded()") < body.indexOf(engine), `${name}: no engine read before the check`);
  }
});
