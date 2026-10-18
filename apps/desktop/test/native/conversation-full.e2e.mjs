import assert from "node:assert/strict";
import { existsSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
assert(evidence, "owned native evidence required");
const mode = process.env.CRAZYAGENT_CONVERSATION_CASE;
assert(["authority", "uncertainty"].includes(mode), "explicit actual fixture mode required");
let sequence = 0;

async function fixture(action) {
  sequence += 1;
  const temporary = join(evidence, `request-${sequence}.pending`);
  const request = join(evidence, `request-${sequence}.json`);
  const reply = join(evidence, `reply-${sequence}.json`);
  writeFileSync(temporary, JSON.stringify({ action }), { flag: "wx", mode: 0o600 });
  renameSync(temporary, request);
  await browser.waitUntil(() => existsSync(reply), { timeout: 15000, timeoutMsg: `actual fixture ${action} response required` });
  assert(statSync(reply).size < 1024 * 1024, "actual observation byte bound");
  return JSON.parse(readFileSync(reply, "utf8"));
}

async function click(selector) {
  await browser.waitUntil(async () => await dom(selector, "enabled"), { timeout: 15000, timeoutMsg: `actual visible enabled control ${selector} required` });
  const actual = await browser.execute((selector) => {
    const node = selector.startsWith("button=")
      ? [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === selector.slice(7))
      : document.querySelector(selector);
    if (!node || node.disabled || !node.getClientRects().length) return false;
    node.click();
    return true;
  }, selector);
  assert.equal(actual, true);
}

async function dom(selector, field) {
  return browser.execute((selector, field) => {
    const node = selector.startsWith("button=")
      ? [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === selector.slice(7))
      : document.querySelector(selector);
    if (field === "exists") return Boolean(node);
    if (field === "enabled") return Boolean(node && !node.disabled && node.getClientRects().length);
    if (!node) throw new Error(`actual DOM node missing: ${selector}`);
    return field === "value" ? node.value : node.textContent.trim();
  }, selector, field);
}

async function value(selector, text) {
  await browser.waitUntil(async () => await dom(selector, "enabled"), { timeout: 10000 });
  assert.equal(await browser.execute((selector, text) => {
    const editor = document.querySelector(selector);
    if (!(editor instanceof HTMLTextAreaElement) || editor.disabled || !editor.getClientRects().length) return false;
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(editor, text);
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  }, selector, text), true);
  await browser.waitUntil(async () => await dom(selector, "value") === text, { timeout: 10000 });
}

async function project(id) {
  await click(`[data-project-id="${id}"]`);
  await browser.waitUntil(async () => (await dom('[data-testid="conversation-freshness"]', "text")).startsWith("Brief read at revision"), { timeout: 15000 });
}

async function notice(text) {
  await browser.waitUntil(async () => (await dom(".notice-region", "text")).includes(text), { timeout: 15000, timeoutMsg: `actual outcome notice ${text} required` });
}

async function idle() {
  await browser.waitUntil(async () => await dom('[data-testid="connect-owner"]', "enabled"), { timeout: 15000 });
}

async function snapshot(expected) {
  let observed;
  await browser.waitUntil(async () => {
    observed = await fixture("snapshot");
    return expected(observed);
  }, { timeout: 15000, timeoutMsg: "independent actual engine state did not match" });
  return observed;
}

async function recover() {
  await idle();
  await click('[data-testid="connect-owner"]');
  await idle();
  await click('[data-testid="reconcile-owner"]');
  await idle();
  if (await dom('[data-testid="retry-owner"]', "exists")) {
    await click('[data-testid="retry-owner"]');
    await idle();
  }
  assert.equal(await dom('[data-testid="reconcile-owner"]', "exists"), false, "actual original intent must settle");
}

describe(`actual native conversation ${mode}`, () => {
  if (mode === "authority") {
    it("persists messages, explicitly versions briefs, isolates drafts and respects real revocation", async () => {
      console.log("conversation-stage: actual project and initial state");
      await click("button=Conversation");
      await project("alpha");
      const initial = await fixture("snapshot");
      assert.deepEqual(initial.alpha.messages, []);
      assert.equal(initial.alpha.brief, null);
      const prefix = "<script>literal owner message</script>\n\u0001\u202e";
      const suffix = "complete end marker";
      const text = prefix + "x".repeat(8192 - Buffer.byteLength(prefix + suffix)) + suffix;
      const displayed = text.replace("\u0001", "\\u0001").replace("\u202e", "\\u202e");
      assert.equal(Buffer.byteLength(text), 8192);
      await value('[data-testid="conversation-message"]', text);
      await click('[data-testid="send-conversation-message"]');
      await snapshot((s) => s.alpha.messages.length === 1 && s.alpha.messages[0] === text);
      await browser.waitUntil(async () => await dom('[data-testid="conversation-message"]', "value") === "", { timeout: 10000 });
      assert.equal(await dom('[data-testid="current-project-brief"]', "text"), "No brief has been recorded.");
      assert.equal(await dom('[data-testid="conversation-entry-text"]', "text"), displayed);
      assert.equal(await browser.execute(() => {
        const node = document.querySelector('[data-testid="conversation-entry-text"]');
        return node.scrollHeight > node.clientHeight && getComputedStyle(node).overflowY === "auto";
      }), true, "complete bounded text must remain scrollable");
      assert.equal(await browser.execute(() => document.querySelectorAll(".conversation-history script").length), 0);
      await value('[data-testid="conversation-message"]', "prior draft");
      assert.equal(await browser.execute(() => {
        const editor = document.querySelector('[data-testid="conversation-message"]');
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(editor, "x".repeat(8193));
        editor.dispatchEvent(new Event("input", { bubbles: true }));
        return true;
      }), true);
      await browser.waitUntil(async () => (await dom(".notice-region", "text")).includes("previous draft is retained"), { timeout: 10000 });
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "prior draft");
      console.log("conversation-stage: actual explicit brief and concurrent version conflict");
      await click('[data-testid="load-current-brief"]');
      await value('[data-testid="conversation-brief"]', "owner explicit brief v1");
      await click('[data-testid="replace-project-brief"]');
      await snapshot((s) => s.alpha.brief?.version === "1" && s.alpha.brief.text === "owner explicit brief v1");
      await idle();
      await click('[data-testid="load-current-brief"]');
      await value('[data-testid="conversation-brief"]', "must reject an old version");
      const independent = await fixture("concurrent_brief");
      assert.equal(independent.alpha.brief.version, "2");
      await click('[data-testid="refresh-conversation"]');
      await idle();
      await click('[data-testid="replace-project-brief"]');
      await notice("stale_brief_version");
      assert.equal(await dom('[data-testid="conversation-brief"]', "value"), "must reject an old version");
      const conflict = await fixture("snapshot");
      assert.equal(conflict.alpha.brief.text, "independent version");
      await idle();
      assert.equal(await dom('[data-testid="replace-project-brief"]', "enabled"), false);
      await click('[data-testid="load-current-brief"]');
      await value('[data-testid="conversation-brief"]', "owner explicit brief v3");
      await click('[data-testid="replace-project-brief"]');
      await snapshot((s) => s.alpha.brief?.version === "3" && s.alpha.brief.text === "owner explicit brief v3");
      console.log("conversation-stage: project and editor isolation, actual paged content");
      await idle();
      await value('[data-testid="conversation-message"]', "alpha retained draft");
      await click('[data-testid="load-current-brief"]');
      await value('[data-testid="conversation-brief"]', "alpha unsent brief draft");
      await project("beta");
      await value('[data-testid="conversation-message"]', "beta retained draft");
      assert.equal(await dom('[data-testid="conversation-brief"]', "value"), "");
      await project("alpha");
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "alpha retained draft");
      assert.equal(await dom('[data-testid="conversation-brief"]', "value"), "alpha unsent brief draft");
      await click("button=Approvals");
      await click("button=Conversation");
      await idle();
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "alpha retained draft");
      assert.equal(await dom('[data-testid="conversation-brief"]', "value"), "alpha unsent brief draft");
      await click('[data-testid="refresh-conversation"]');
      await idle();
      assert.equal(await dom('[data-testid="conversation-entry-text"]', "text"), displayed);
      await click('[data-testid="next-conversation"]');
      await idle();
      assert.equal(await dom('[data-testid="conversation-entry-text"]', "text"), "owner explicit brief v1");
      console.log("conversation-stage: actual revoke, other-kind continuity and reactivation");
      const revoked = await fixture("revoke");
      assert.equal(revoked.active, false);
      await click('[data-testid="inspect-project"]');
      await browser.waitUntil(async () => (await dom('[data-testid="conversation-freshness"]', "text")).includes("stale or unavailable"), { timeout: 10000 });
      assert.equal(await dom('[data-testid="send-conversation-message"]', "enabled"), false);
      const stillActual = await fixture("snapshot");
      assert.equal(stillActual.alpha.brief.version, "3");
      assert.deepEqual(stillActual.beta.messages, []);
      const active = await fixture("reactivate");
      assert.equal(active.active, true);
      assert(BigInt(active.epoch) > BigInt(revoked.epoch));
      await click('[data-testid="connect-owner"]');
      await idle();
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "alpha retained draft");
      assert.equal(await dom('[data-testid="conversation-brief"]', "value"), "alpha unsent brief draft");
      await click('[data-testid="send-conversation-message"]');
      const final = await snapshot((s) => s.alpha.messages.length === 2);
      assert.deepEqual(final.alpha.messages, [text, "alpha retained draft"]);
      assert.equal(final.alpha.brief.text, "owner explicit brief v3");
      assert.deepEqual(final.beta.messages, []);
      await project("constructor");
      await value('[data-testid="conversation-message"]', "valid external project key draft");
      await project("alpha");
      await project("constructor");
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "valid external project key draft");
      await browser.saveScreenshot(join(evidence, "conversation-authority.png"));
      console.log("conversation-stage: authority workflow completed");
    });

    it("reconstructs authoritative conversation after an actual native reload", async () => {
      await idle();
      await browser.refresh();
      await click("button=Conversation");
      await project("alpha");
      assert.equal(await dom('[data-testid="current-project-brief"]', "text"), "owner explicit brief v3");
      await snapshot((s) => s.alpha.messages.length === 2 && s.alpha.brief?.version === "3");
      await browser.saveScreenshot(join(evidence, "conversation-reload.png"));
    });
  } else {
    it("retains exact original uncertain intent and newer identical/different drafts through other views", async () => {
      await click("button=Conversation");
      await project("alpha");
      const original = "original queued alpha message";
      await value('[data-testid="conversation-message"]', original);
      assert.equal((await fixture("pause")).stopped, true);
      await click('[data-testid="send-conversation-message"]');
      await value('[data-testid="conversation-message"]', "intermediate newer edit");
      await value('[data-testid="conversation-message"]', original);
      await click('[data-project-id="beta"]');
      await value('[data-testid="conversation-message"]', "beta while alpha is pending");
      await click("button=Approvals");
      await notice("intent is retained");
      assert((await dom(".notice-region", "text")).includes("alpha"));
      assert.equal((await fixture("resume")).stopped, false);
      await click("button=Conversation");
      await recover();
      const recorded = await snapshot((s) => s.alpha.messages.length === 1);
      assert.deepEqual(recorded.alpha.messages, [original]);
      assert.deepEqual(recorded.beta.messages, []);
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "beta while alpha is pending");
      await project("alpha");
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), original, "newer identical replacement must survive original receipt");
      console.log("conversation-stage: identical replacement and project isolation completed");
      await value('[data-testid="conversation-message"]', "second queued original");
      assert.equal((await fixture("pause")).stopped, true);
      await click('[data-testid="send-conversation-message"]');
      await value('[data-testid="conversation-message"]', "different newer draft must remain");
      await notice("intent is retained");
      assert.equal((await fixture("resume")).stopped, false);
      await recover();
      const final = await snapshot((s) => s.alpha.messages.length === 2);
      assert.deepEqual(final.alpha.messages, [original, "second queued original"]);
      assert.deepEqual(final.beta.messages, []);
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "different newer draft must remain");
      await click("button=Approvals");
      await idle();
      await click(`[data-baseline-id="${final.baseline.id}"]`);
      await idle();
      assert.equal((await fixture("pause")).stopped, true);
      await click('[data-testid="approve-baseline"]');
      await click("button=Conversation");
      await notice("intent is retained");
      assert((await dom(".notice-region", "text")).includes(final.baseline.id));
      await value('[data-testid="conversation-message"]', "draft while baseline is pending");
      assert.equal((await fixture("resume")).stopped, false);
      await recover();
      const approved = await snapshot((s) => s.baseline.approved);
      assert.equal(approved.baseline.revoked, false);
      assert.deepEqual(approved.alpha.messages, [original, "second queued original"]);
      assert.deepEqual(approved.beta.messages, []);
      assert.equal(await dom('[data-testid="conversation-message"]', "value"), "draft while baseline is pending");
      await browser.saveScreenshot(join(evidence, "conversation-uncertainty.png"));
      console.log("conversation-stage: queued late delivery and baseline pending across views completed, no commit-before-timeout claim");
    });
  }
});
