import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

const api = await import("../src/api.ts");
const projects = await import("../src/api-projects.ts");
const client = await import("../src/engine-client.ts");
const research = await import("../src/screens/project/research.ts");
const { updateMessage } = await import("../src/screens/settings/updates-state.ts");
const { signInFailure } = await import("../src/screens/settings/sign-in.tsx");

const ENGINE = new URL("../../../crates/crazyagent/src/", import.meta.url).pathname;
const FALLBACK = "Something went wrong. Try again.";
const PLAIN = /^[a-z0-9_]{1,96}$/;

function production(path) {
  const lines = readFileSync(path, "utf8").split("\n");
  const kept = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^#\[cfg\((all\()?test/.test(lines[index].trim()) && /^(pub(\(crate\))? )?mod [a-z_]+ \{/.test((lines[index + 1] ?? "").trim())) break;
    kept.push(lines[index]);
  }
  return kept.join("\n");
}

function sources(folder) {
  return readdirSync(folder).flatMap((name) => {
    const path = join(folder, name);
    if (statSync(path).isDirectory()) return name === "tui" || name === "tests" ? [] : sources(path);
    return name.endsWith(".rs") && name !== "tests.rs" && !name.endsWith("_tests.rs") ? [path] : [];
  });
}

function literals(text, pattern) {
  return [...text.matchAll(pattern)].map((match) => match[1]);
}

/**
 * Finds the bracket that closes a call or a body. Takes the source text, the index just after
 * the opening bracket and the bracket pair, parentheses unless given. Returns the index just
 * after its matching close.
 */
function closing(text, start, open = "(", close = ")") {
  let depth = 1;
  let index = start;
  while (depth > 0 && index < text.length) {
    if (text[index] === open) depth += 1;
    if (text[index] === close) depth -= 1;
    index += 1;
  }
  assert.equal(depth, 0, `unclosed ${open} at ${start}`);
  return index;
}

/**
 * Reads every code literal a call can pass, including each branch of an if or match argument.
 * Takes the source text and a global pattern that ends at the call's opening parenthesis.
 * Returns the code literals found between that parenthesis and its matching close.
 */
function callLiterals(text, call) {
  return [...text.matchAll(call)].flatMap((match) => {
    const start = match.index + match[0].length;
    return literals(text.slice(start, closing(text, start) - 1), /"([a-z][a-z0-9_]*)"/g);
  });
}

const PRINT = /\be?print(?:ln)?!\(/g;

/**
 * Drops every print call from Rust source, so a code literal that only labels a log line is
 * not read as a code the engine records. Takes the source text. Returns the rest of it.
 */
function withoutPrints(text) {
  let kept = "";
  let from = 0;
  for (const match of text.matchAll(PRINT)) {
    if (match.index < from) continue;
    kept += text.slice(from, match.index);
    from = closing(text, match.index + match[0].length);
  }
  return kept + text.slice(from);
}

const CODE_FN = /\bfn code\(&?self\)\s*->\s*&'static str\s*\{/g;

/**
 * Reads the sign-in codes every `fn code` of an engine failure type returns, one per match
 * arm. Takes the source text. Returns the `native_auth_` literals inside each such body.
 */
function codeArms(text) {
  return [...text.matchAll(CODE_FN)].flatMap((match) => {
    const start = match.index + match[0].length;
    return literals(text.slice(start, closing(text, start, "{", "}") - 1), /"(native_auth_[a-z_]+)"/g);
  });
}

const REJECT = /\breject(?:ed)?\(/g;
const engineFiles = sources(ENGINE).map((path) => ({ path: path.slice(ENGINE.length), text: production(path) }));
const refusals = new Set(engineFiles.flatMap(({ text }) => [
  ...callLiterals(text, REJECT),
  ...literals(text, /Rejected\s*\{\s*code:\s*"([a-z][a-z0-9_]*)"/g),
]));
const masterStorage = production(join(ENGINE, "storage/master_session.rs"));
const selection = readFileSync(join(ENGINE, "master_session.rs"), "utf8");
const validate = selection.slice(selection.indexOf("pub(crate) fn validate_selection("), selection.indexOf("\n}\n", selection.indexOf("pub(crate) fn validate_selection(")));
const transport = readFileSync(join(ENGINE, "engine/owner/transport.rs"), "utf8");
const replyFrame = transport.slice(transport.indexOf("fn owner_reply_frame("), transport.indexOf("let response", transport.indexOf("fn owner_reply_frame(")));
const sendCodes = new Set([
  ...callLiterals(masterStorage, REJECT),
  ...literals(validate, /\berror\(\s*"([a-z][a-z0-9_]*)"/g),
  ...literals(replyFrame, /"([a-z][a-z0-9_]*)"/g),
]);
const MASTER_PATH = /(^|\/)(master[a-z_]*|codex_host|claude_host|model_gateway|native_profile)[a-z_]*(\/|\.rs$)/;
const turnCodes = new Set(engineFiles.filter(({ path }) => MASTER_PATH.test(path)).flatMap(({ text }) =>
  literals(text, /\b(?:failure|error|EngineError)\(\s*"([a-z][a-z0-9_]*)"/g)));
const reasonEnum = selection.slice(selection.indexOf("pub enum MasterReason {"), selection.indexOf("}", selection.indexOf("pub enum MasterReason {")));
const reasons = literals(reasonEnum, /^\s+([A-Z][A-Za-z]+),$/gm).map((name) => name.replace(/[A-Z]/g, (letter, at) => `${at ? "_" : ""}${letter.toLowerCase()}`));

test("the engine scan finds each owner path's codes", () => {
  assert.ok(refusals.size >= 100, `owner refusals: ${refusals.size}`);
  assert.ok(sendCodes.size >= 20, `master send refusals: ${sendCodes.size}`);
  assert.ok(turnCodes.size >= 150, `master turn failures: ${turnCodes.size}`);
  assert.ok(reasons.length >= 20 && reasons.includes("unknown") && reasons.includes("readiness_unknown"), reasons.join(","));
  for (const code of [...refusals, ...sendCodes, ...turnCodes]) assert.match(code, PLAIN, "every engine code passes the bridge as itself");
  assert.ok(sendCodes.has("master_turn_outstanding") && sendCodes.has("owner_busy"));
  assert.ok(turnCodes.has("claude_host_deadline") && turnCodes.has("gateway_no_eligible_account"));
});

test("a code without its own sentence shows the plain fallback and the code name, never alone", () => {
  assert.equal(api.plainError("made_up_code"), `${FALLBACK} (made_up_code)`);
  assert.equal(api.plainError("raw text <b>"), `${FALLBACK} (unknown)`);
  assert.equal(api.plainError("engine_unreachable"), "CrazyAgent's engine isn't running.");
  assert.equal(projects.failureCopy("made_up_code"), `${FALLBACK} (made_up_code)`);
  assert.equal(client.errorMessage({ code: "made_up_code" }), "The owner request failed. Refresh before continuing. (made_up_code)");
  assert.equal(updateMessage("something_new"), "Something went wrong with the update. Try again. (something_new)");
  assert.equal(research.sendFailureLine("made_up_code"), "Couldn't send. Try again. (made_up_code)");
});

test("a bridge failure keeps its own sentence and names the engine code behind it", () => {
  const failed = api.classifyFailure({ code: "owner_read_failed", cause: "project_missing" });
  assert.equal(failed.code, "owner_read_failed");
  assert.equal(failed.cause, "project_missing");
  assert.equal(failed.message, `${api.plainError("owner_read_failed")} (project_missing)`);
  assert.equal(projects.failureCopy(failed), `${projects.failureCopy("owner_read_failed")} (project_missing)`);
  assert.equal(api.classifyFailure({ code: "owner_read_failed", cause: "C:\\Users\\mira" }).message, `${api.plainError("owner_read_failed")} (unknown)`);
  assert.equal(api.classifyFailure({ code: "owner_read_failed", cause: "owner_read_failed" }).message, api.plainError("owner_read_failed"));
  assert.equal(api.classifyFailure("some raw panic text with /Users/secret/path").message, `${FALLBACK} (unknown)`);
  assert.equal(api.classifyFailure({ code: "made_up_code", cause: "engine_detail" }).message, `${FALLBACK} (engine_detail)`);
});

/**
 * Collects every sign-in failure code the engine records: the custodian codes it keeps, each
 * engine failure type's `fn code` arms and its Some(...) and .unwrap_or(...) literals.
 * Takes nothing. Returns the codes, after checking each source was found.
 */
function recordedSignInCodes() {
  const listed = production(join(ENGINE, "storage/native_auth.rs")).match(/\bCUSTODIAN_CODES: \[&str; \d+\] = \[([^\]]*)\]/);
  assert.ok(listed, "storage/native_auth.rs lists the codes a custodian may name");
  const custodian = literals(listed[1], /"([a-z][a-z0-9_]*)"/g);
  const arms = engineFiles.flatMap(({ text }) => codeArms(text));
  assert.ok(custodian.length > 0 && arms.length > 0, `${custodian.length} custodian codes, ${arms.length} failure arms`);
  const facts = new Set([
    ...custodian,
    ...arms,
    ...engineFiles.flatMap(({ text }) => literals(withoutPrints(text), /(?:Some|\.unwrap_or)\(\s*"(native_auth_[a-z_]+)"/g)),
  ]);
  assert.ok(facts.size >= 6 && facts.has("native_auth_interrupted"), [...facts].join(","));
  return facts;
}

test("every sign-in failure the engine records reads as a sentence, and an unknown one names its code", () => {
  for (const code of recordedSignInCodes()) {
    const { text } = signInFailure(code, "chatgpt");
    assert.doesNotMatch(text, / \([a-z0-9_]+\)$|_/, `${code}: ${text}`);
  }
  assert.equal(signInFailure("native_auth_made_up", "claude").text, "Sign-in didn't finish. Try again. (native_auth_made_up)");
});

test("each sign-in sentence is true on every path that records its code, and an inherited key never reads as one", () => {
  const sentences = [
    ["native_auth_codex_exited_before_observation", "Codex stopped before it finished. Try again."],
    ["native_auth_codex_deadline", "Codex didn't answer in time. Try again."],
    ["native_auth_codex_protocol_invalid", "Codex gave an answer CrazyAgent couldn't use. Update Codex, then try again."],
    ["native_auth_codex_io_failed", "CrazyAgent lost contact with Codex. Try again."],
    ["native_auth_bootstrap_timeout", "CrazyAgent's account helper didn't start in time. Try again."],
    ["native_auth_claude_native_exit", "Claude Code stopped before it finished. Try again."],
    ["native_auth_claude_status_invalid", "Claude Code's account check didn't give an answer CrazyAgent could use. Update Claude Code, then try again."],
    ["native_auth_claude_output_invalid", "Claude Code didn't finish because of a problem CrazyAgent couldn't handle. Try again, and update Claude Code if it keeps happening."],
    ["native_auth_claude_profile_mismatch", "Claude Code's settings folder for this account didn't match the one CrazyAgent set up. Try again, and update Claude Code if it keeps happening."],
    ["native_auth_claude_launch_failed", "Claude Code couldn't start. Check that Claude Code is installed and opens on this Mac, then try again."],
    ["native_auth_claude_keychain_unavailable", "CrazyAgent couldn't set up this account's own keychain for Claude Code. Try again. If it keeps failing, delete the account, then add it again."],
    ["native_auth_claude_destination_unsupported", "Claude Code gave a sign-in page address CrazyAgent doesn't recognize. Update CrazyAgent and Claude Code, then try again."],
    ["native_auth_claude_credential_expiring", "Claude Code couldn't renew this account's sign-in before it expires. Try again."],
    ["native_profile_identity_changed", "This account's sign-in folder no longer matches what CrazyAgent recorded, so CrazyAgent stopped. Delete the account, then add it again."],
  ];
  for (const [code, text] of sentences) assert.deepEqual(signInFailure(code, "claude"), { text, hint: "retry" }, code);
  assert.deepEqual(signInFailure("native_auth_host_executable_unsafe", "chatgpt"),
    { text: "Codex on this Mac isn't safe to start, so CrazyAgent didn't start it. Reinstall Codex, then try again.", hint: "retry" });
  assert.deepEqual(signInFailure("native_auth_host_executable_changed", "claude"),
    { text: "Claude Code changed on this Mac after CrazyAgent checked it, perhaps from an update. Try again.", hint: "retry" });
  for (const code of ["__proto__", "constructor", "toString", "valueOf"]) {
    assert.deepEqual(signInFailure(code, "claude"), { text: client.withCode("Sign-in didn't finish. Try again.", code), hint: "retry" }, code);
  }
});

const TUI_SIGN_IN = "tui/screens/settings_add_account.rs";
const TUI_OWN = new Set(["native_auth_recovery_required"]);

test("the TUI gives every sign-in failure the engine records the app's sentence", () => {
  const source = readFileSync(join(ENGINE, TUI_SIGN_IN), "utf8");
  const start = source.indexOf("const SIGN_IN_SENTENCES");
  assert.ok(start >= 0, `${TUI_SIGN_IN} keeps its sign-in sentences in SIGN_IN_SENTENCES`);
  const table = source.slice(start, source.indexOf("];", start));
  const rows = [...table.matchAll(/\(\s*"([a-z][a-z0-9_]*)",\s*"((?:[^"\\]|\\.)*)",?\s*\)/g)].map((match) => [match[1], match[2]]);
  const sentences = new Map(rows);
  assert.equal(sentences.size, rows.length, "each code is listed once");
  for (const code of new Set([...recordedSignInCodes(), ...sentences.keys()])) {
    assert.ok(sentences.has(code), `${code} has no TUI sentence`);
    for (const [provider, harness, name] of [["chatgpt", "Codex", "ChatGPT"], ["claude", "Claude Code", "Claude"]]) {
      const line = sentences.get(code).replaceAll("{harness}", harness).replaceAll("{name}", name);
      if (TUI_OWN.has(code)) assert.doesNotMatch(line, / \([a-z0-9_]+\)$|_|[{}]/, `${code}: ${line}`);
      else assert.equal(line, signInFailure(code, provider).text, `${code} for ${provider}`);
    }
  }
});

test("every refusal the engine can give an owner change reads as a sentence or names its code", () => {
  for (const code of refusals) {
    const line = projects.failureCopy(code);
    assert.ok(line === `${FALLBACK} (${code})` || !line.includes(FALLBACK), `${code}: ${line}`);
    assert.notEqual(line, FALLBACK, code);
  }
});

test("every refusal of a master send reads as Couldn't send plus a sentence or the code", () => {
  for (const code of sendCodes) {
    const line = research.sendFailureLine(code);
    assert.notEqual(line, "Couldn't send. Try again.", code);
    assert.ok(line === `Couldn't send. Try again. (${code})` || line === `Couldn't send. ${projects.failureCopy(code)}`, `${code}: ${line}`);
  }
});

test("every master stop reads as a sentence that ends with the code the turn stopped on", () => {
  const turn = (phase, reason, cause) => ({ id: "t", ordinal: 1, phase, reason, reply: null, ...(cause ? { cause } : {}) });
  for (const reason of reasons) {
    for (const phase of ["failed", "blocked"]) {
      const line = research.turnLine(turn(phase, reason));
      const sentence = line.slice(0, line.lastIndexOf(" ("));
      assert.doesNotMatch(sentence, /_/, `${phase} ${reason}: ${line}`);
      if (reason.includes("_")) assert.doesNotMatch(sentence, new RegExp(reason.replaceAll("_", " ")), `${phase} ${reason} reads as its enum name: ${line}`);
      assert.match(line, /^[A-Z].*\. \([a-z0-9_]+\)$/, line);
      assert.ok(line.endsWith(` (${reason === "unknown" ? phase : reason})`), line);
    }
  }
  for (const reason of reasons.filter((name) => name !== "unknown" && name !== "approval_required")) {
    for (const phase of ["failed", "blocked"]) {
      assert.notEqual(research.turnLine(turn(phase, reason)), `The master stopped. (${reason})`, `${phase} ${reason} says why the master stopped`);
    }
  }
  for (const code of turnCodes) {
    assert.match(research.turnLine(turn("failed", "unknown", code)), new RegExp(`^The master stopped.*\\. \\(${code}\\)$`), code);
  }
  assert.equal(research.turnLine(turn("failed", "unknown")), "The master stopped. (failed)");
  assert.equal(research.turnLine(turn("blocked", "readiness_unknown", "master_authentication_unknown")),
    "Not signed in. Sign in again in Settings, Accounts, then send again. (master_authentication_unknown)");
});

test("a configured master stopped by a harness protocol fault, the frame size limit or an unchecked worktree says what to do next (F39)", () => {
  const turn = (reason, cause) => ({ id: "t", ordinal: 1, phase: "failed", reason, reply: null, cause });
  assert.equal(research.turnLine(turn("protocol_fault", "codex_host_protocol_stale_message")),
    "The master stopped: it sent a message out of order. Send again to start a fresh turn. (codex_host_protocol_stale_message)");
  assert.equal(research.turnLine(turn("frame_limit", "codex_host_protocol_frame_too_large")),
    "The master stopped: one message was larger than the master frame size limit. Raise it in Settings, Master, then send again. (codex_host_protocol_frame_too_large)");
  assert.equal(research.turnLine(turn("worktree_uncertain", "master_bootstrap_uncertain")),
    "The master stopped: its copy of the project could not be checked. End the conversation, then send again. (master_bootstrap_uncertain)");
});

test("a team agent or task that closed on a launch code shows the engine line with its code", () => {
  const closed = "The agent process closed. (launch_protocol_failed)";
  const stopped = "The sub-agent stopped unexpectedly. (launch_input_closed)";
  const agent = { id: 2, role: "sub", harness: "codex", tools: false, account: null, worktree: null, status: "failed", status_label: "Failed",
    task: null, turns: 0, problem: closed, process_group: null, created_ms: 1, heartbeat_ms: 1, stopped_ms: 2 };
  const task = { id: 7, source: "master", harness: "codex", tools: false, prompt: "Write the page", status: "failed", status_label: "Failed", agent: 2,
    attempts: 3, result: null, problem: stopped, delivered: false, output_bytes: 0, created_ms: 1, started_ms: 1, finished_ms: 2 };
  const panel = research.teamPanel({ agents: [agent], tasks: [task] }, [], (id) => id, "all", 3);
  assert.equal(panel.agents[0].task, closed);
  assert.equal(panel.tasks[0].summary, stopped);
  const runtime = production(join(ENGINE, "team/runtime.rs"));
  assert.match(runtime, /with_code\(PROCESS_CLOSED, cause\.as_deref\(\)\)/, "the engine names the launch code an agent closed on");
  assert.match(runtime, /with_code\("The sub-agent stopped unexpectedly", cause\)/, "and the code a task stopped on");
  assert.doesNotMatch(runtime, /"The agent could not start \(\{\}\)"/, "and never quotes raw launch text");
});

test("an agent start refused for skills left in its Codex home says what happened", () => {
  const sentence = client.errorMessage({ code: "launch_context_skills_kept" });
  assert.equal(sentence, "The agent could not start: an earlier run locked skills into its Codex home, so CrazyAgent could not remove them and Codex would have loaded them.");
  const runtime = production(join(ENGINE, "team/runtime.rs"));
  const worded = runtime.match(/const SKILLS_KEPT_PROBLEM: &str = "((?:[^"\\]|\\\n)*)";/);
  assert.ok(worded, "the team start line words the refusal");
  assert.equal(sentence, `${worded[1].replace(/\\\n\s*/g, "")}.`, "the desktop and the team panels give the same sentence");
});
