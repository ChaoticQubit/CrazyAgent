import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { keyArgs, startTui } from "./tui-kit.mjs";

const ECHO = "process.stdin.setEncoding(\"utf8\"); process.stdout.write(\"ready\\n\"); process.stdin.on(\"data\", (line) => process.stdout.write(`got ${line}`));";
const RAW = "process.stdin.setRawMode(true); process.stdout.write(\"raw\\n\"); process.stdin.on(\"data\", (bytes) => process.stdout.write(`hex ${bytes.toString(\"hex\")}\\n`));";
const KEY = "sk-ant-api03-" + "Ab1".repeat(14);

/** Whether a private tmux server still answers. Input: its socket name. Output: a boolean. */
function alive(socket) {
  return spawnSync("tmux", ["-L", socket, "has-session"], { encoding: "utf8" }).status === 0;
}

/** Runs a test body against a node program in the TUI kit, then stops it and removes the run
 * folder. Inputs: the program source and the body, given the kit and the folder. */
async function withTui(source, body, options = {}) {
  const evidence = realpathSync(mkdtempSync(join(tmpdir(), "ca-tui-kit-")));
  const tui = await startTui({ argv: [process.execPath, "-e", source], env: { PATH: process.env.PATH, HOME: evidence, LANG: "en_US.UTF-8" }, evidence, ...options });
  try {
    await body(tui, evidence);
  } finally {
    await tui.stop();
    rmSync(evidence, { recursive: true, force: true });
  }
  assert.ok(!alive(tui.socket), "stop ends the private tmux server");
}

test("the TUI kit runs a program in its own tmux server, types, reads the pane, waits and saves masked frames", async () => {
  await withTui(ECHO, async (tui, evidence) => {
    assert.match(tui.socket, /^ca-trial-tui-[0-9]+-[0-9]+$/);
    assert.ok(alive(tui.socket));
    await tui.waitFor("ready", 10000);
    await tui.type("hello");
    await tui.key("Enter");
    await tui.waitFor(/got hello$/m, 10000);
    await tui.type(KEY);
    await tui.key("Enter");
    await tui.waitFor(`got ${KEY}`, 10000);
    assert.match(tui.pane(), /got hello\n/);
    const frame = tui.capture("05-n-t1-1-two-lines");
    assert.equal(frame.path, "tui4/05-n-t1-1-two-lines.txt");
    assert.ok(!existsSync(join(evidence, "frames")), "the runner's recorder alone writes frames/");
    const saved = readFileSync(join(evidence, frame.path), "utf8");
    assert.match(saved, /got hello/);
    assert.doesNotMatch(saved, /sk-ant-api03-Ab1/);
    assert.equal(frame.text, saved);
    await assert.rejects(tui.waitFor("never shown", 600), /the TUI never showed never shown within 0\.6 s; last pane:\n.*got hello/s);
    assert.throws(() => tui.capture("../escape"), /frame name/);
  });
});

test("keys reach the program as the TUI reads them: Shift-Enter as CSI u, Ctrl-K, Enter, arrows and letters", async () => {
  await withTui(RAW, async (tui) => {
    await tui.waitFor("raw", 10000);
    for (const [name, hex] of [["Shift-Enter", "1b5b31333b3275"], ["Ctrl-K", "0b"], ["Enter", "0d"], ["Down", "1b5b42"], ["l", "6c"], ["Esc", "1b"], ["Ctrl-]", "1d"]]) {
      await tui.key(name);
      await tui.waitFor(new RegExp(`^hex ${hex}$`, "m"), 10000);
    }
  });
});

test("keyArgs names tmux keys and refuses a name that is not one; go refuses an unknown place or a missing project", async () => {
  assert.deepEqual(keyArgs("Shift-Tab"), ["BTab"]);
  assert.deepEqual(keyArgs("Shift-Left"), ["S-Left"]);
  assert.deepEqual(keyArgs("Ctrl-T"), ["C-t"]);
  assert.deepEqual(keyArgs("y"), ["-l", "y"]);
  assert.throws(() => keyArgs("Enter; kill-server"), /a key name looks like/);
  assert.throws(() => keyArgs(""), /a key name looks like/);
  await assert.rejects(startTui({ argv: ["crazyagent", "tui"], env: {}, evidence: tmpdir() }), /argv\[0\] must be an absolute path/);
  await withTui(ECHO, async (tui) => {
    await assert.rejects(tui.go("project-master"), /go needs the project name/);
  });
  await withTui(ECHO, async (tui) => {
    await assert.rejects(tui.go("home"), /go takes one of project-master, project-team, got home/);
  }, { project: "ca-trial-4" });
});
