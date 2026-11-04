import assert from "node:assert/strict";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { startTui } from "./tui-kit.mjs";

const HOME = `
let view = "home";
let sel = 0;
let name = "";
let msg = "";
let typed = "";
let open = "";
let esc = 0;
const P = "ca-trial-4";
const E = "\\u001b";
const cards = [P + "-readd-05", P];
function draw(crumbs, footer, body) {
  process.stdout.write(E + "[2J" + E + "[H");
  process.stdout.write(" [logo] " + crumbs.join("  /  ") + "\\n");
  process.stdout.write((body || "") + "\\n\\n");
  process.stdout.write(footer + "\\n");
}
function list(bars) {
  return cards.map((c, i) => (bars && i === sel ? "\\u258c " : "  ") + c + "    ready").join("\\n");
}
function render() {
  if (view === "home") draw(["CrazyAgent", "Home"], "n  New project    /  Filter    Ctrl-K  Go to    Enter  Open", list(true));
  else if (view === "filter") draw(["CrazyAgent", "Home"], "Enter  Keep filter    Esc  Clear filter", "/ " + typed + "\\n" + list(false));
  else if (view === "overview") draw(["CrazyAgent", "Home", open], "[  Prev    ]  Next    Esc  Home", "  Overview of the project");
  else if (view === "conversation") draw(["CrazyAgent", "Home", open, "Conversation"], "Ctrl-T  Team    Enter  Send", "  conversation " + msg);
  else if (view === "master") draw(["CrazyAgent", open, "Master"], "Enter  Send", "  master terminal");
  else if (view === "team") draw(["CrazyAgent", open, "Team"], "h  Switch harness", "  team settings");
  else if (view === "newproject") draw(["CrazyAgent", "Home", "New project", "About"], "Tab  Next field", "  Name\\n  [ " + name + " ]");
}
function down() {
  if (view === "home") sel = Math.min(sel + 1, cards.length - 1);
}
function onByte(b) {
  if (esc === 1) { esc = b === 0x5b ? 2 : 0; return; }
  if (esc === 2) { if (b === 0x42) down(); esc = 0; return; }
  if (b === 0x1b) { esc = 1; return; }
  if (b === 0x2f) {
    if (view === "home") { view = "filter"; }
    else if (view === "conversation") { msg += "/"; }
    return;
  }
  if (b === 0x0d) {
    if (view === "filter") { view = "home"; }
    else if (view === "home") { open = cards[sel]; view = "overview"; }
    else if (view === "conversation") { if (msg === "/master") view = "master"; msg = ""; }
    return;
  }
  if (b === 0x5d) { if (view === "overview") view = "conversation"; return; }
  if (b === 0x14) { if (view === "conversation") view = "team"; return; }
  if (b >= 0x20 && b < 0x7f) {
    const c = String.fromCharCode(b);
    if (view === "home" && c === "n") view = "newproject";
    else if (view === "filter") { typed += c; }
    else if (view === "conversation") { msg += c; }
    else if (view === "newproject") { name += c; }
  }
}
process.stdin.setRawMode(true);
render();
process.stdin.on("data", (d) => { for (const b of d) onByte(b); render(); });
`;

async function withHome(body) {
  const evidence = realpathSync(mkdtempSync(join(tmpdir(), "ca-tui-open-")));
  const tui = await startTui({
    argv: [process.execPath, "-e", HOME],
    env: { PATH: process.env.PATH, HOME: evidence, LANG: "en_US.UTF-8" },
    evidence,
    project: "ca-trial-4",
    waitMs: 8000,
  });
  try {
    await body(tui);
  } finally {
    await tui.stop();
    rmSync(evidence, { recursive: true, force: true });
  }
}

test("the stand-in reproduces the live leak: a name typed on Home opens New project and the rest lands in the Name field", async () => {
  await withHome(async (tui) => {
    await tui.waitFor("ca-trial-4", 8000);
    await tui.type("Conversation");
    const pane = await tui.waitFor("New project", 8000);
    assert.match(pane, /New project {2}\/ {2}About/);
    assert.match(pane, /Name\n {2}\[ versation \]/);
  });
});

test("go('project-master') walks past the re-add sibling to the exact project and opens Master", async () => {
  await withHome(async (tui) => {
    const pane = await tui.go("project-master");
    assert.match(pane.split("\n", 1)[0], /\/ {2}ca-trial-4 {2}\/ {2}Master$/);
    assert.match(pane, /master terminal/);
    assert.doesNotMatch(pane, /readd/);
    assert.doesNotMatch(pane, /New project/);
  });
});

test("go('project-team') opens the Team screen through the Conversation tab of the exact project", async () => {
  await withHome(async (tui) => {
    const pane = await tui.go("project-team");
    assert.match(pane.split("\n", 1)[0], /\/ {2}ca-trial-4 {2}\/ {2}Team$/);
    assert.match(pane, /team settings/);
    assert.doesNotMatch(pane, /readd/);
  });
});
