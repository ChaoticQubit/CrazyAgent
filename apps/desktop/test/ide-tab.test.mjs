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
    assert.ok(markup.includes(line), `${build.state}: ${markup}`);
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
  assert.match(source, /set_menu\(ide_menu\(app\)/, "showing the IDE installs its menu");
  assert.match(source, /set_menu\(Menu::default\(app\)/, "hiding it gives the app its own menu back");
});

test("the IDE webview runs the input guard, locks navigation and opens other links outside", () => {
  const source = read("src-tauri/src/ide.rs");
  assert.match(source, /include_str!\("ide_guard\.js"\)/);
  assert.match(source, /\.initialization_script\(GUARD\)/);
  assert.match(source, /\.on_navigation\(/);
  assert.match(source, /\.on_new_window\(/);
  assert.match(source, /NewWindowResponse::Deny/);
  assert.match(source, /open::open_link/);
  const allowed = JSON.parse(read("src-tauri/capabilities/main-window.json"));
  assert.deepEqual(allowed.webviews, ["main"], "the IDE webview gets no app command");
});

test("the IDE tab follows its panel, hides with the tab, closes with the project and beats every minute", () => {
  const source = read("src/screens/project/ide.tsx");
  assert.equal(client.HEARTBEAT_MS, 60_000);
  assert.match(source, /new ResizeObserver\(/);
  assert.match(source, /hideIde\(/);
  assert.match(source, /touchIde\(/);
  const index = read("src/screens/project/index.tsx");
  assert.match(index, /closeIde\(projectId\)/, "leaving the project closes its IDE webview");
  assert.deepEqual(client.bounds({ left: 10.4, top: 80.6, width: 900.2, height: 600.7 }), { x: 10, y: 81, width: 900, height: 601 });
  assert.equal(client.bounds({ left: 0, top: 0, width: 0, height: 300 }), null, "a hidden panel has no bounds");
});
