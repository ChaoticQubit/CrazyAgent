import assert from "node:assert/strict";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { startTui } from "./tui-kit.mjs";

const HOME = `
let state = "home";
let name = "";
let msg = "";
const P = "ca-trial-4";
const E = "\\u001b";
function draw(crumbs, footer, body) {
  process.stdout.write(E + "[2J" + E + "[H");
  process.stdout.write("  " + crumbs.join("  /  ") + "\\n");
  process.stdout.write((body || "") + "\\n\\n");
  process.stdout.write(footer + "\\n");
}
function render() {
  if (state === "home") draw(["CrazyAgent", "Home"], "n  New project    /  Filter    Ctrl-K  Go to    Enter  Open", "  > " + P + "\\n    another project");
  else if (state === "filter") draw(["CrazyAgent", "Home"], "Enter  Keep filter    Esc  Clear filter", "  > " + P);
  else if (state === "overview") draw(["CrazyAgent", P, "Overview"], "[  Prev    ]  Next    Esc  Home", "  Overview of the project");
  else if (state === "conversation") draw(["CrazyAgent", P, "Conversation"], "Ctrl-T  Team    Enter  Send", "  conversation " + msg);
  else if (state === "master") draw(["CrazyAgent", P, "Master"], "Enter  Send", "  master terminal");
  else if (state === "team") draw(["CrazyAgent", P, "Team"], "h  Switch harness", "  team settings");
  else if (state === "newproject") draw(["CrazyAgent", "Home", "New project", "About"], "Tab  Next field", "  Name\\n  [ " + name + " ]");
}
process.stdin.setRawMode(true);
render();
process.stdin.on("data", (d) => {
  for (const b of d) {
    if (b === 0x2f) {
      if (state === "home") state = "filter";
      else if (state === "conversation") msg += "/";
    } else if (b === 0x0d) {
      if (state === "filter") state = "home";
      else if (state === "home") state = "overview";
      else if (state === "conversation") { if (msg === "/master") state = "master"; msg = ""; }
    } else if (b === 0x5d) {
      if (state === "overview") state = "conversation";
    } else if (b === 0x14) {
      if (state === "conversation") state = "team";
    } else if (b >= 0x20 && b < 0x7f) {
      const c = String.fromCharCode(b);
      if (state === "home" && c === "n") state = "newproject";
      else if (state === "conversation") msg += c;
      else if (state === "newproject") name += c;
    }
  }
  render();
});
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

test("go('project-master') opens the existing project from Home without leaking into New project", async () => {
  await withHome(async (tui) => {
    const pane = await tui.go("project-master");
    assert.match(pane.split("\n", 1)[0], /ca-trial-4 {2}\/ {2}Master$/);
    assert.match(pane, /master terminal/);
    assert.doesNotMatch(pane, /New project/);
  });
});

test("go('project-team') opens the Team screen from Home through the Conversation tab", async () => {
  await withHome(async (tui) => {
    const pane = await tui.go("project-team");
    assert.match(pane.split("\n", 1)[0], /ca-trial-4 {2}\/ {2}Team$/);
    assert.match(pane, /team settings/);
    assert.doesNotMatch(pane, /New project/);
  });
});
