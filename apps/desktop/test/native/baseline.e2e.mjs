import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { join } from "node:path";

const execute = promisify(execFile);
const id = process.env.CRAZYAGENT_DESKTOP_TEST_BASELINE;
const fixture = process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE;
const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
assert(id && fixture && evidence, "real native fixture readiness required");

async function observed() {
  const { stdout } = await execute(fixture, ["observe", id], {
    env: process.env,
    timeout: 8000,
    maxBuffer: 4096,
    killSignal: "SIGKILL",
  });
  const state = JSON.parse(stdout);
  assert.equal(state.baseline_id, id);
  return state;
}

async function clickActualButton(selector) {
  await globalThis.browser.execute((selector) => {
    const element = document.querySelector(selector);
    if (!(element instanceof HTMLButtonElement) || element.disabled || element.offsetParent === null) {
      throw new Error("actual visible enabled button required");
    }
    element.click();
    return true;
  }, selector);
}

async function inspectClosedBoundary() {
  await globalThis.browser.execute(() => {
    document.documentElement.dataset.ownerBoundary = "running";
    const invoke = (command, args) => window.__TAURI_INTERNALS__.invoke(command, args);
    const probe = async () => {
      const status = await invoke("desktop_status");
      if (status.connected !== true || status.pending !== false) {
        throw new Error("actual admitted native status required");
      }
      const refused = [];
      for (const [command, args] of [
        ["desktop_engine_request", { request: { operation: "Apply" } }],
        ["plugin:shell|execute", { program: "not-an-authorized-program" }],
        ["desktop_approve_baseline", {
          ticket: "not-a-native-selection",
          anchor: "not-a-native-anchor",
          request: { operation: "Apply", mutation: "RevokeBaseline" },
        }],
      ]) {
        try {
          await invoke(command, args);
          refused.push(false);
        } catch (error) {
          refused.push(command !== "desktop_approve_baseline" || error?.code === "stale_selection");
        }
      }
      document.documentElement.dataset.ownerBoundary = JSON.stringify(refused);
    };
    void probe().catch(() => { document.documentElement.dataset.ownerBoundary = "failed"; });
    return true;
  });
  await globalThis.browser.waitUntil(async () =>
    await globalThis.browser.execute(() => document.documentElement.dataset.ownerBoundary) === "[true,true,true]",
    { timeout: 8000, timeoutMsg: "actual closed native IPC did not deny injected operations" });
}

async function inspectAfterRendererReload() {
  const { browser } = globalThis;
  await browser.execute(() => { window.location.reload(); });
  await browser.waitUntil(async () => await browser.execute((id) => {
    const row = document.querySelector(`[data-baseline-id="${id}"]`);
    return row instanceof HTMLButtonElement && !row.disabled;
  }, id), { timeout: 8000, timeoutMsg: "real renderer reload did not reconstruct the baseline page" });
  await clickActualButton(`[data-baseline-id="${id}"]`);
  await browser.waitUntil(async () => await browser.execute(() =>
    document.querySelector('[data-testid="baseline-state"]')?.textContent === "Revoked"),
    { timeout: 8000, timeoutMsg: "real renderer reload lost the authoritative revoked state" });
  const requirements = await browser.execute(() =>
    document.querySelector('[data-testid="baseline-requirements"]')?.textContent);
  assert.equal(requirements, "Inspect <script>untrusted text</script> as literal text");
  const state = await observed();
  assert.equal(state.approved, true);
  assert.equal(state.revoked, true);
  await browser.saveScreenshot(join(evidence, "native-reloaded.png"));
}

describe("native paired baseline owner flow", function () {
  it("inspects exact content and independently verifies approval and revocation", async function () {
    const { browser, $ } = globalThis;
    console.log("native-phase: real baseline row");
    const row = await $(`[data-baseline-id="${id}"]`);
    await row.waitForDisplayed();
    console.log("native-phase: baseline row displayed");
    await inspectClosedBoundary();
    console.log("native-phase: actual closed IPC rejects injected operations");
    await clickActualButton(`[data-baseline-id="${id}"]`);
    console.log("native-phase: baseline row clicked");
    const requirements = await $('[data-testid="baseline-requirements"]');
    await requirements.waitForDisplayed();
    console.log("native-phase: requirements displayed");
    assert.equal(await requirements.getText(),
      "Inspect <script>untrusted text</script> as literal text");
    console.log("native-phase: exact literal requirements");
    const scriptNodes = await browser.execute(() =>
      document.querySelector('[data-testid="baseline-requirements"]').querySelectorAll("script").length);
    assert.equal(scriptNodes, 0);
    console.log("native-phase: actual DOM contains no script nodes");
    const before = await observed();
    assert.equal(before.approved, false);
    assert.equal(before.revoked, false);
    console.log("native-phase: independent real engine initial state");
    await browser.saveScreenshot(join(evidence, "native-inspection.png"));
    const approve = await $('[data-testid="approve-baseline"]');
    console.log("native-phase: inspect actual approval control");
    assert.equal(await approve.isEnabled(), true, "fresh native baseline must support approval");
    await clickActualButton('[data-testid="approve-baseline"]');
    await browser.waitUntil(async () => (await observed()).approved,
      { timeout: 8000, timeoutMsg: "actual engine did not approve baseline" });
    console.log("native-phase: independent real engine approval effect");
    const badge = await $('[data-testid="baseline-state"]');
    await browser.waitUntil(async () =>
      await badge.getText() === "Approved",
      { timeout: 8000, timeoutMsg: "native UI did not display actual approval" });
    assert.equal(await badge.getText(), "Approved");
    console.log("native-phase: native UI displays actual approval");
    await browser.saveScreenshot(join(evidence, "native-approved.png"));
    const revoke = await $('[data-testid="revoke-baseline"]');
    assert.equal(await revoke.isEnabled(), true);
    await clickActualButton('[data-testid="revoke-baseline"]');
    await browser.waitUntil(async () => (await observed()).revoked,
      { timeout: 8000, timeoutMsg: "actual engine did not revoke baseline" });
    console.log("native-phase: independent real engine revocation effect");
    const after = await observed();
    assert.equal(after.approved, true);
    assert.equal(after.revoked, true);
    await browser.waitUntil(async () =>
      await badge.getText() === "Revoked",
      { timeout: 8000, timeoutMsg: "native UI did not display actual revocation" });
    assert.equal(await badge.getText(), "Revoked");
    console.log("native-phase: native UI displays effective revocation with approval history");
    await browser.saveScreenshot(join(evidence, "native-revoked.png"));
    await inspectAfterRendererReload();
    console.log("native-phase: real renderer reload reconstructs authoritative revoked history");
  });
});
