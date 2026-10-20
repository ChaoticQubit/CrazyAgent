import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { basename, isAbsolute, join } from "node:path";
import { ftruncateSync, lstatSync, openSync, writeFileSync, writeSync } from "node:fs";

const execute = promisify(execFile);
const panel = '[data-testid="native-auth-panel"]';
const first = "native-auth-connection-a";
const second = "native-auth-connection-b";
const account = "native-auth-account";
const stages = new Set();
let progressDescriptor;
let progressStage = "case.prepare";
let progressStarted;

function progress(label) {
  const allowed = ["case.prepare", "case.initial", "case.start", "case.navigate", "case.cancel",
    "case.logout", "case.complete", "idle.wait", "click.find", "click.enabled", "click.action",
    "click.idle", "input.find", "input.enabled", "input.set", "input.value", "save.click",
    "save.wait", "connection.create", "connection.fields", "connection.endpoint", "connection.auth",
    "connection.profile", "connection.save", "prepare.idle", "prepare.settings",
    "prepare.authority", "prepare.account.create", "prepare.account.input", "prepare.account.save",
    "prepare.connections", "prepare.first", "prepare.second", "fields.execute", "observe.exec",
    "open.select", "open.panel", "open.display", "open.context", "read.click", "read.observe",
    "read.fields", "initial.open", "initial.control", "initial.logout", "initial.confirm",
    "initial.back", "initial.operation", "initial.confirm-again", "initial.escape",
    "initial.closed", "initial.reopen", "initial.proof", "start.click", "navigate.escape",
    "cancel.click", "logout.open"];
  if (!allowed.includes(label)) throw new Error("closed auth progress label required");
  if (label.startsWith("case.") || label.startsWith("prepare.")) progressStage = label;
  progressStarted ??= performance.now();
  const elapsedMs = Math.floor(performance.now() - progressStarted);
  const bytes = Buffer.from(JSON.stringify({
    stage: progressStage, step: label, elapsed_ms: elapsedMs,
  }));
  if (bytes.length > 256) throw new Error("auth progress byte bound");
  if (progressDescriptor === undefined) {
    progressDescriptor = openSync(join(privateRun().evidence, "native-auth-progress.json"),
      "wx", 0o600);
  }
  if (writeSync(progressDescriptor, bytes, 0, bytes.length, 0) !== bytes.length) {
    throw new Error("auth progress write incomplete");
  }
  ftruncateSync(progressDescriptor, bytes.length);
}

function privateRun() {
  const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
  const fixture = process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE;
  assert(process.env.CRAZYAGENT_DESKTOP_TEST_SUITE === "native-auth",
    "closed auth suite required");
  assert(evidence && isAbsolute(evidence) && fixture && isAbsolute(fixture),
    "declared absolute evidence and fixture paths required");
  const info = lstatSync(evidence);
  assert(info.isDirectory() && info.uid === process.getuid() && (info.mode & 0o077) === 0,
    "owned private evidence directory required");
  const run = basename(evidence);
  assert(/^run-[A-Za-z0-9_-]{1,58}$/.test(run), "new bounded private run required");
  return { evidence, fixture, run };
}

function proof(stage, observation) {
  assert(["initial", "start-ui", "start", "read", "selection", "cancel", "logout",
    "reconnect"].includes(stage) && !stages.has(stage), "one closed proof per auth stage");
  stages.add(stage);
  assert(stages.size <= 8, "auth proof count bound");
  const bytes = JSON.stringify({ stage, sign_in_qualified: false, observation });
  assert(Buffer.byteLength(bytes) <= 4096, "nonsecret auth proof byte bound");
  writeFileSync(join(privateRun().evidence, `native-auth-${stage}.json`), bytes,
    { flag: "wx", mode: 0o600 });
}

async function idle() {
  progress("idle.wait");
  await browser.waitUntil(async () => await $('[data-testid="connect-owner"]').isEnabled(),
    { timeout: 15000, timeoutMsg: "actual owner action did not settle" });
}

async function click(selector, root) {
  progress("click.find");
  const element = root ? await $(root).$(selector) : await $(selector);
  progress("click.enabled");
  await element.waitForEnabled({ timeout: 10000 });
  progress("click.action");
  await element.click();
  progress("click.idle");
  await idle();
}

async function input(field, value) {
  progress("input.find");
  const element = await $(`[data-testid="model-field-${field}"]`);
  progress("input.enabled");
  await element.waitForEnabled({ timeout: 10000 });
  progress("input.set");
  await element.setValue(value);
  progress("input.value");
  assert(await element.getValue() === value, "real metadata input did not retain its value");
}

async function save() {
  progress("save.click");
  await click('[data-testid="save-model-access"]');
  progress("save.wait");
  await browser.waitUntil(async () =>
    !await $('[data-testid="model-access-form"]').isExisting(),
  { timeout: 10000, timeoutMsg: "actual owner metadata mutation did not settle" });
}

async function createConnection(id, suffix) {
  progress("connection.create");
  await click('[data-testid="create-model-access"]');
  const fields = { id, provider: "native-auth-provider", harness: "codex", protocol: "native",
    account, model: "native-auth-model", author: "native-auth-author" };
  progress("connection.fields");
  for (const [field, value] of Object.entries(fields)) await input(field, value);
  progress("connection.endpoint");
  await $('[data-testid="model-field-endpoint"]').selectByAttribute("value", "native");
  progress("connection.auth");
  await $('[data-testid="model-field-auth"]').selectByAttribute("value", "native_managed");
  progress("connection.profile");
  await input("credential", `auth-${privateRun().run}-${suffix}`);
  progress("connection.save");
  await save();
}

async function prepare() {
  privateRun();
  const handles = await browser.getWindowHandles();
  assert(handles.length === 1 && typeof handles[0] === "string" && handles[0].length > 0,
    "single live handle required");
  assert(handles[0] === "main", "single main label required");
  await browser.switchToWindow(handles[0]);
  assert(await browser.getWindowHandle() === handles[0], "selected main handle required");
  progress("prepare.idle");
  await idle();
  progress("prepare.settings");
  await click("button=Settings");
  progress("prepare.authority");
  await browser.waitUntil(async () =>
    (await $('[data-testid="model-access-authority"]').getText()).startsWith("Writable:"),
  { timeout: 10000, timeoutMsg: "actual writable metadata owner required" });
  progress("prepare.account.create");
  await click('[data-testid="create-model-access"]');
  progress("prepare.account.input");
  await input("id", account);
  progress("prepare.account.save");
  await save();
  progress("prepare.connections");
  await click('[data-testid="model-tab-connections"]');
  progress("prepare.first");
  await createConnection(first, "a");
  progress("prepare.second");
  await createConnection(second, "b");
}

async function fields() {
  progress("fields.execute");
  return browser.execute(() => {
    const root = document.querySelector('[data-testid="native-auth-panel"]');
    if (!root) return null;
    const result = {};
    const allowed = ["Selected connection", "Configuration", "Settings source",
      "Original operation", "Original connection", "Owner phase", "Owner generation",
      "Owner database", "Engine epoch"];
    for (const label of allowed) {
      const term = Array.from(root.querySelectorAll("dt"))
        .find((element) => element.textContent === label);
      const value = term?.nextElementSibling?.textContent;
      result[label] = typeof value === "string" && value.length <= 128 ? value : null;
    }
    const alert = root.querySelector('[role="alert"]')?.textContent ?? "";
    result.reason = /Reason: ([a-z_]{1,128})$/.exec(alert)?.[1] ?? null;
    result.presentation_present = Array.from(root.querySelectorAll("dt,h3,p")).some((element) =>
      ["Browser sign-in URL", "Device code", "Verification URL", "Codex ChatGPT observation",
        "Codex Amazon Bedrock observation", "Claude observation", "Codex API key observed."]
        .includes(element.textContent ?? ""));
    return result;
  });
}

async function observe(connection, operation) {
  const args = ["observe-native-auth", connection];
  assert([first, second].includes(connection), "closed observer connection required");
  if (operation !== undefined) {
    assert(typeof operation === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(operation),
      "actual bounded original operation required");
    args.push(operation);
  }
  let result;
  try {
    progress("observe.exec");
    result = await execute(privateRun().fixture, args,
      { timeout: 10000, killSignal: "SIGKILL", maxBuffer: 4096 });
  } catch {
    throw new Error("genuine Desktop OwnerClient auth observation failed; no alternate scope");
  }
  assert(result.stderr === "" && Buffer.byteLength(result.stdout) <= 4096,
    "bounded observer output without backend diagnostics required");
  const value = JSON.parse(result.stdout);
  assert(value.observer_kind === "desktop" && value.connection === connection && value.writable,
    "genuine Desktop owner must inspect the selected writable connection");
  for (const key of ["config_id", "source_hash", "database_instance", "engine_epoch"]) {
    assert(typeof value[key] === "string" && /^[0-9a-f]{64}$/.test(value[key]),
      "actual owner identity and source hashes required");
  }
  assert(/^(0|[1-9][0-9]*)$/.test(value.revision), "actual owner revision required");
  assert((operation === undefined) === (value.operation === null),
    "operation observation must match the requested read");
  return value;
}

function contextMatches(ui, actual) {
  assert(ui && ui["Selected connection"] === actual.connection &&
    ui.Configuration === actual.config_id && ui["Settings source"] === actual.source_hash,
  "visible selected connection must join actual owner configuration and source");
}

function factMatches(ui, actual, original, action) {
  contextMatches(ui, actual);
  const fact = actual.operation;
  assert(fact && fact.operation_id === original && fact.connection_match &&
    fact.connection === actual.connection && fact.config_id === actual.config_id &&
    fact.source_hash === actual.source_hash &&
    fact.database_instance === actual.database_instance &&
    fact.engine_epoch === actual.engine_epoch && /^[1-9][0-9]{0,19}$/.test(fact.generation),
  "original operation must join actual owner binding and generation");
  assert(ui["Original operation"] === fact.operation_id &&
    ui["Original connection"] === fact.connection && ui["Owner phase"] === fact.phase &&
    ui["Owner generation"] === fact.generation && ui["Owner database"] === fact.database_instance &&
    ui["Engine epoch"] === fact.engine_epoch,
  "visible operation must equal independent owner fact");
  assert(fact.action.kind === action && (action !== "sign_in" ||
    fact.action.method === "codex_browser"), "actual owner action must retain original intent");
  assert(fact.phase === "unavailable" && fact.code === "native_auth_host_unavailable" &&
    !fact.challenge_present && !fact.account_present && !ui.presentation_present,
  "frozen no-effect owner closure required; this flow does not qualify sign-in");
}

async function open(connection = first) {
  progress("open.select");
  await click(`[data-model-id="${connection}"]`);
  progress("open.panel");
  await click('[data-testid="native-auth-open"]');
  progress("open.display");
  await $(panel).waitForDisplayed({ timeout: 10000 });
  progress("open.context");
  contextMatches(await fields(), await observe(connection));
}

async function readOriginal(original, action = "sign_in") {
  progress("read.click");
  await click('[data-testid="native-auth-read"]');
  progress("read.observe");
  const actual = await observe(first, original);
  progress("read.fields");
  factMatches(await fields(), actual, original, action);
  return actual;
}

async function initialControls() {
  progress("initial.open");
  await open();
  for (const name of ["read", "cancel"]) {
    progress("initial.control");
    assert(!await $(`[data-testid="native-auth-${name}"]`).isEnabled(),
      "Read and Cancel require an actual retained operation");
  }
  progress("initial.logout");
  await click("button=Logout…", panel);
  progress("initial.confirm");
  assert(await $(panel).$("button=Confirm logout").isEnabled(),
    "logout requires visible explicit confirmation");
  progress("initial.back");
  await click("button=Back", panel);
  progress("initial.operation");
  assert((await fields())["Original operation"] === null,
    "Back must not invent or submit an operation");
  progress("initial.confirm-again");
  await click("button=Logout…", panel);
  progress("initial.escape");
  await browser.keys("Escape");
  progress("initial.closed");
  assert(!await $(panel).isExisting(),
    "Escape from the focused confirmation must hide the panel");
  progress("initial.reopen");
  await open();
  progress("initial.proof");
  proof("initial", { selected: await observe(first), ui: await fields() });
}

async function startOriginal() {
  progress("start.click");
  await click("button=Start browser sign-in", panel);
  await browser.waitUntil(async () => {
    const ui = await fields();
    return Boolean(ui?.["Original operation"] || ui?.reason);
  }, { timeout: 10000, timeoutMsg: "Start returned neither an owner operation nor a safe reason" });
  const ui = await fields();
  proof("start-ui", ui);
  const original = ui?.["Original operation"];
  assert(original,
    "real Start must reach the owner; inspect native command declarations and grants");
  const actual = await observe(first, original);
  factMatches(ui, actual, original, "sign_in");
  assert(actual.operation.generation === "1", "fresh private profile must have no prior operation");
  proof("start", actual);
  proof("read", await readOriginal(original));
  return original;
}

async function navigate(original) {
  progress("navigate.escape");
  await browser.keys("Escape");
  assert(!await $(panel).isExisting(), "focused Read Escape must hide the panel");
  await click("button=Approvals");
  await click("button=Settings");
  assert(!await $(panel).isExisting(), "navigation must not reopen ephemeral auth presentation");
  await open(second);
  const ui = await fields();
  assert(ui["Original operation"] === original && ui["Original connection"] === first,
    "selection change must preserve the original identity");
  for (const name of ["read", "cancel"]) {
    assert(!await $(`[data-testid="native-auth-${name}"]`).isEnabled(),
      "a different selected connection must not read or cancel the original");
  }
  proof("selection", { ui, original: await observe(first, original) });
  await open();
  await readOriginal(original);
  await click('[data-testid="refresh-model-access"]');
  assert(!await $(panel).isExisting(), "metadata refresh must hide presentation");
  await open();
  await readOriginal(original);
}

async function cancelOriginal(original) {
  progress("cancel.click");
  await click('[data-testid="native-auth-cancel"]');
  const ui = await fields();
  assert(ui?.reason === "native_auth_not_active" && ui["Original operation"] === original,
    "terminal Cancel must expose the genuine not-active reason and original identity");
  assert(!await $('[data-testid="native-auth-read"]').isEnabled(),
    "a failed Cancel must invalidate the current selected basis");
  const actual = await observe(first, original);
  factMatches(ui, actual, original, "sign_in");
  proof("cancel", actual);
  await open();
  await readOriginal(original);
}

async function logoutAndReconnect(original) {
  progress("logout.open");
  await click("button=Logout…", panel);
  await click("button=Back", panel);
  factMatches(await fields(), await observe(first, original), original, "sign_in");
  await click("button=Logout…", panel);
  await click("button=Confirm logout", panel);
  const ui = await fields();
  const logout = ui?.["Original operation"];
  assert(logout && logout !== original, "confirmed logout must retain its own new operation ID");
  const actual = await observe(first, logout);
  factMatches(ui, actual, logout, "logout");
  assert(actual.operation.generation === "2", "confirmed logout must advance the actual profile");
  const prior = await observe(first, original);
  assert(prior.operation.operation_id === original && prior.operation.generation === "1" &&
    prior.operation.action.kind === "sign_in", "original sign-in fact must remain unchanged");
  proof("logout", { actual, prior });
  await click('[data-testid="connect-owner"]');
  const reconciled = await observe(first, logout);
  factMatches(await fields(), reconciled, logout, "logout");
  proof("reconnect", reconciled);
  await click("button=Close", panel);
  assert(!await $(panel).isExisting(), "Close must hide the panel without claiming cancellation");
}

describe("actual native desktop authentication controls", () => {
  it("joins real owner operations through Settings without claiming native sign-in", async () => {
    progress("case.prepare");
    await prepare();
    progress("case.initial");
    await initialControls();
    progress("case.start");
    const original = await startOriginal();
    progress("case.navigate");
    await navigate(original);
    progress("case.cancel");
    await cancelOriginal(original);
    progress("case.logout");
    await logoutAndReconnect(original);
    progress("case.complete");
  });
});
