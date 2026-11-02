import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { click, finishWelcome, observeModelAccess, pick, says, shot, shown, toasts, type, until } from "./settings-steps.mjs";

const execute = promisify(execFile);
const CODEX_MISSING = "Codex isn't installed on this Mac, so ChatGPT sign-in isn't available. Install Codex, then check again in Settings, Harnesses.";
const CLAUDE_MISSING = "Claude Code isn't installed on this Mac, so Claude sign-in isn't available. Install Claude Code, then check again in Settings, Harnesses.";

async function observeSignIn(connection) {
  const result = await execute(process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE, ["observe-native-auth", connection],
    { timeout: 10000, killSignal: "SIGKILL", maxBuffer: 8192 });
  assert.equal(result.stderr, "", "the sign-in observation ran without diagnostics");
  const value = JSON.parse(result.stdout);
  assert.equal(value.observer_kind, "desktop");
  assert.equal(value.connection, connection);
  assert.equal(value.writable, true);
  return value;
}

async function startUnavailable(tile, method, name, start, missing) {
  await pick("tile", tile);
  await pick("method", method);
  await until(() => shown("#account-name"), 15000, `${method} did not open`);
  await type("#account-name", name);
  await click(`button=${start}`);
  await until(() => says("dialog[open] .notice[role=alert]", missing), 30000, `${method} did not report the missing harness`);
  assert.equal(await shown("button=Use an API key instead"), true, "the owner is offered a key instead");
  const engine = await observeModelAccess();
  assert.equal(engine.connections.length, 1, "starting a sign-in saved the account first");
  const [connection] = engine.connections;
  assert.deepEqual(connection.endpoint, { kind: "native" });
  assert.equal(connection.auth.method, "native_managed");
  assert.deepEqual(engine.registry.map((account) => [account.id, account.name]), [[connection.id, name]]);
  await observeSignIn(connection.id);
  return connection;
}

async function removedAgain(name) {
  const removed = `${name} wasn't signed in, so it was removed.`;
  await until(async () => JSON.stringify(await toasts()) === JSON.stringify([removed]), 15000, `the toast did not say exactly ${removed}`);
  const engine = await observeModelAccess();
  assert.deepEqual([engine.registry, engine.accounts, engine.connections], [[], [], []]);
}

describe("Settings sign-in on the real desktop app and an engine without a sign-in host", () => {
  it("first run ends on Add account", async () => {
    await finishWelcome("Sign In Owner");
    const engine = await observeModelAccess();
    assert.deepEqual([engine.registry, engine.connections], [[], []]);
  });

  it("ChatGPT sign-in says Codex is missing, and closing keeps no unfinished account", async () => {
    const connection = await startUnavailable("OpenAI", "Sign in with ChatGPT", "ChatGPT check", "Get sign-in code", CODEX_MISSING);
    assert.match(connection.id, /^chatgpt-account-[0-9a-f]{20}$/);
    await shot("codex-missing");
    await click("dialog[open] button[aria-label='Close']");
    await removedAgain("ChatGPT check");
    await until(() => says("main .empty-state h2", "No accounts yet"), 15000, "the account list was not empty");
  });

  it("Claude sign-in says Claude Code is missing, and a key instead drops the unfinished account", async () => {
    await click("button=Add account");
    const connection = await startUnavailable("Anthropic", "Sign in with your Claude subscription", "Claude check", "Open Claude sign-in", CLAUDE_MISSING);
    assert.match(connection.id, /^claude-account-[0-9a-f]{20}$/);
    await shot("claude-missing");
    await click("button=Use an API key instead");
    await until(() => shown("#account-key"), 15000, "the key form did not open");
    await removedAgain("Claude check");
    await click("dialog[open] button[aria-label='Close']");
  });
});
