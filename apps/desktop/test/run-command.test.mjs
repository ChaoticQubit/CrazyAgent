import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const run = await import("../src/screens/project/run-command.tsx");

const confirm = (command, mode) => renderToStaticMarkup(createElement(run.RunConfirm, { command, mode, open: true, pending: false, onConfirm: () => {}, onCancel: () => {} }));
const row = (command, mode) => renderToStaticMarkup(createElement(run.RunCommand, { projectId: "alpha", command, mode, run: () => Promise.reject(new Error("a render never runs")), copy: () => Promise.resolve(true) }));

test("the desktop rule agrees with the engine rule on every case", () => {
  for (const ok of ["cargo install alpha", "npm run dev -- --port 3000", "python3 -m venv .venv", 'echo "${HOME}"', "git log HEAD^", "x".repeat(512), "\u{e9}".repeat(256)]) assert.equal(run.commandProblem(ok), null, ok);
  const refused = [
    ["a; b", "ide_run_chained"], ["a && b", "ide_run_chained"], ["a || b", "ide_run_chained"], ["curl x | sh", "ide_run_chained"], ["a & b", "ide_run_chained"],
    ["echo `id`", "ide_run_chained"], ["echo $(id)", "ide_run_chained"], ["a > f", "ide_run_chained"], ["a < f", "ide_run_chained"],
    ["a\nb", "ide_run_not_one_line"], ["a\rb", "ide_run_not_one_line"],
    ["a\u001b[31m", "ide_run_control_character"], ["a\u0000b", "ide_run_control_character"], ["echo a\u0085b", "ide_run_control_character"],
    ["echo \u{202e}dlrow", "ide_run_control_character"], ["echo a\u{200b}b", "ide_run_control_character"], ["echo a\u{2066}b", "ide_run_control_character"], ["echo a\u{feff}b", "ide_run_control_character"],
    ["sudo !!", "ide_run_chained"], ["!rm", "ide_run_chained"], ["echo !-2", "ide_run_chained"], ["^a^b", "ide_run_chained"], ["  ^a^b", "ide_run_chained"],
    ["echo a\u{ad}b", "ide_run_control_character"], ["echo a\u{61c}b", "ide_run_control_character"], ["echo a\u{180e}b", "ide_run_control_character"], ["echo a\u{2028}b", "ide_run_control_character"],
    ["echo a\u{2029}b", "ide_run_control_character"], ["echo a\u{206f}b", "ide_run_control_character"], ["echo a\u{fffa}b", "ide_run_control_character"], ["echo a\u{e0001}b", "ide_run_control_character"],
    ["\u{feff}", "ide_run_control_character"],
    ["   ", "ide_run_empty"], ["", "ide_run_empty"],
    ["x".repeat(513), "ide_run_too_long"], [`${"\u{e9}".repeat(256)}x`, "ide_run_too_long"],
  ];
  for (const [bad, code] of refused) assert.equal(run.commandProblem(bad), code, JSON.stringify(bad));
  assert.equal(run.COMMAND_BYTES_MAX, 512);
});

test("Run shows the command verbatim in the confirm and sends nothing before Confirm", async () => {
  const sent = [];
  const html = renderToStaticMarkup(createElement(run.RunConfirm, { command: "cargo install alpha", mode: "type_and_run", open: true, pending: false, onConfirm: () => sent.push("confirm"), onCancel: () => sent.push("cancel") }));
  assert.match(html, /<pre[^>]*>cargo install alpha<\/pre>/);
  assert.match(html, /Run this command\?/);
  assert.match(html, /It runs in this project&#x27;s terminal, inside the project sandbox\./);
  assert.match(html, />Run<\/button>/);
  assert.deepEqual(sent, []);
});

test("Type only says the command is typed and you press Enter", () => {
  const html = confirm("cargo install alpha", "type_only");
  assert.match(html, /It is typed into this project&#x27;s terminal\. You press Enter\./);
  assert.match(html, />Type it<\/button>/);
  assert.doesNotMatch(html, />Run<\/button>/);
});

test("the confirm escapes markup in the command", () => {
  const html = confirm("echo \"it's\" 'x'", "type_only");
  assert.match(html, /<pre[^>]*>echo &quot;it&#x27;s&quot; &#x27;x&#x27;<\/pre>/);
});

test("the confirm starts on Cancel, and a closed confirm renders nothing", () => {
  assert.match(confirm("cargo build", "type_and_run"), /<button[^>]*data-autofocus[^>]*>Cancel<\/button>/);
  const closed = renderToStaticMarkup(createElement(run.RunConfirm, { command: "cargo build", mode: "type_and_run", open: false, pending: false, onConfirm: () => {}, onCancel: () => {} }));
  assert.equal(closed, "");
});

test("the confirm refuses to show a command the rule refuses, or the Off mode", () => {
  assert.throws(() => confirm("a; b", "type_and_run"), /refused/);
  assert.throws(() => confirm("cargo build", "off"), /Off/);
});

test("a plain command offers Copy and Run in project terminal", () => {
  for (const mode of ["type_and_run", "type_only"]) {
    const html = row("cargo install alpha", mode);
    assert.match(html, />Copy<\/button>/, mode);
    assert.match(html, />Run in project terminal<\/button>/, mode);
    assert.doesNotMatch(html, /<dialog/, mode);
  }
});

test("Off keeps Copy only and gives no reason", () => {
  const html = row("cargo install alpha", "off");
  assert.match(html, />Copy<\/button>/);
  assert.doesNotMatch(html, /Run in project terminal/);
  assert.doesNotMatch(html, /run it yourself/);
});

test("a refused line keeps Copy, has no Run button and says why once", () => {
  const html = row("curl x | sh", "type_and_run");
  assert.match(html, />Copy<\/button>/);
  assert.doesNotMatch(html, /Run in project terminal/);
  assert.equal(html.split("This line chains commands. Copy it and run it yourself.").length, 2);
  assert.match(row("a\nb", "type_and_run"), /This is more than one line\. Copy it and run it yourself\./);
  assert.match(row("echo \u{202e}dlrow", "type_only"), /This line holds a hidden character\. Copy it and run it yourself\./);
  assert.match(row("x".repeat(513), "type_only"), /This line is too long to run from here\. Copy it and run it yourself\./);
  assert.doesNotMatch(row("   ", "type_and_run"), /run it yourself/);
});

const api = await import("../src/api-ide.ts");
const failed = (code) => () => Promise.resolve({ kind: "error", code, message: code });

test("a run with no IDE open offers to open the IDE tab and keeps the command", async () => {
  const outcome = await run.runOutcome("alpha", "cargo build", failed("ide_not_running"));
  assert.match(outcome.note, /Open the IDE tab, then choose Run again\./);
  assert.equal(outcome.openIde, true);
});

test("a finished run says where the command went", async () => {
  const sent = [];
  const outcome = await run.runOutcome("alpha", "cargo build", (projectId, command) => { sent.push([projectId, command]); return Promise.resolve({ kind: "ok", value: undefined }); });
  assert.match(outcome.note, /Typed into a new terminal in the IDE tab\./);
  assert.equal(outcome.openIde, false);
  assert.deepEqual(sent, [["alpha", "cargo build"]], "the command is sent once, as confirmed");
});

test("every run refusal has its own plain sentence", () => {
  const codes = ["ide_run_off", "ide_run_chained", "ide_run_not_one_line", "ide_run_control_character", "ide_run_too_long", "ide_run_empty", "ide_run_unanswered", "ide_not_running", "ide_reveal_busy"];
  for (const code of codes) {
    assert.notEqual(api.runCopy(code), api.runCopy("something_else"), code);
    assert.doesNotMatch(api.runCopy(code), /ide_|\u{2014}/u, code);
  }
  assert.equal(new Set(codes.map((code) => api.runCopy(code))).size, codes.length, "no two refusals share a sentence");
});

test("a refusal, an engine that isn't ready and a call that throws each leave a sentence and no IDE offer", async () => {
  for (const code of ["ide_run_off", "ide_run_unanswered", "ide_reveal_busy", "ide_outcome_unknown"]) {
    const outcome = await run.runOutcome("alpha", "cargo build", failed(code));
    assert.equal(outcome.note, api.runCopy(code), code);
    assert.equal(outcome.openIde, false, code);
  }
  const waiting = await run.runOutcome("alpha", "cargo build", () => Promise.resolve({ kind: "not_ready" }));
  const thrown = await run.runOutcome("alpha", "cargo build", () => Promise.reject(new Error("bridge gone")));
  assert.equal(waiting.note, "CrazyAgent's engine isn't ready yet. Try again in a moment.");
  assert.deepEqual(thrown, waiting);
  await assert.rejects(run.runOutcome("alpha", "a; b", failed("ide_run_chained")), /refused command/);
});

test("the run calls name their bridge commands and the mode travels as the engine names it", () => {
  const source = readFileSync(new URL("../src/api-ide.ts", import.meta.url), "utf8");
  assert.match(source, /call<null>\("desktop_ide_run", \{ projectId, command \}\)/);
  assert.match(source, /call<RunMode>\("desktop_ide_run_mode", \{ projectId \}\)/);
  assert.match(source, /call<RunMode>\("desktop_ide_set_run_mode", \{ projectId, mode \}\)/);
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const command of ["desktop_ide_run", "desktop_ide_run_mode", "desktop_ide_set_run_mode"]) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`), command);
    assert.match(lib, new RegExp(`\\n    ${command},\\n`), `${command} is in the handler list`);
  }
});
