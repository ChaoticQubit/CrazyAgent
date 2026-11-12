import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { click, finishWelcome, says, settingsTab, shot, shown, texts, type, until } from "./settings-steps.mjs";
import { command } from "./trial4-accounts.mjs";

const UI_MS = 15000;
const ENGINE_MS = 45000;
const DAY_MS = 86400000;
const ALPHA = "Alpha";
const BRAVO = "Bravo";
const POOL = "Spend pool";
const CONFIRM = "Spend confirmation";
const LIMITED = "When every pool account is limited";
const SPEND = "Use a free reset";
const NOTICE = `Every account in ${POOL} is at its limit. Free resets: ${BRAVO}.`;
const QUEUED = "Using a free reset now.";
const NOT_SIGNED_IN = "This account wasn't signed in or was busy. Sign in again in Accounts, then try again.";
const OFF_HELP = "CrazyAgent never uses a free reset. Use them in the provider's own app.";
const ids = {};

/** Runs one read or write on the fixture engine's own store. Input: what to run with the store.
 * Output: what it returned. */
function inStore(work) {
  const store = new DatabaseSync(join(dirname(process.env.CRAZYAGENT_DESKTOP_SOCKET), "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    return work(store);
  } finally {
    store.close();
  }
}

/** Writes both accounts at their weekly limit as a poll would, and Bravo's free reset count as a
 * read of it would, all read now. A usage report comes first, so the tables exist. */
async function seedLimited() {
  await command("desktop_usage_report", { projectId: null, sinceMs: null, offsetMinutes: 0 });
  const now = Date.now();
  inStore((store) => {
    const limits = store.prepare(`INSERT INTO account_limits (account,plan,weekly_percent,weekly_resets_ms,source,observed_ms,checked_ms,weekly_polled_percent,weekly_polled_resets_ms)
      VALUES (?,'test',100,?,'poll',?,?,100,?)
      ON CONFLICT(account) DO UPDATE SET weekly_percent=100,weekly_resets_ms=excluded.weekly_resets_ms,source='poll',observed_ms=excluded.observed_ms,
        checked_ms=excluded.checked_ms,failure=NULL,weekly_polled_percent=100,weekly_polled_resets_ms=excluded.weekly_polled_resets_ms`);
    for (const name of [ALPHA, BRAVO]) limits.run(ids[name], now + 3 * DAY_MS, now, now, now + 3 * DAY_MS);
    store.prepare(`INSERT INTO account_limit_extras (account,resets_count,resets_checked_ms) VALUES (?,2,?)
      ON CONFLICT(account) DO UPDATE SET resets_count=2,resets_checked_ms=excluded.resets_checked_ms`).run(ids[BRAVO], now);
  });
}

/** Reads every spend attempt the engine stores. Output: account, origin, state and outcome of
 * each, oldest first; never the key. */
function attempts() {
  return inStore((store) => store.prepare("SELECT account,origin,state,outcome FROM usage_spend_attempts ORDER BY id").all())
    .map((row) => ({ account: row.account, origin: row.origin, state: row.state, outcome: row.outcome }));
}

/** Waits until no spend attempt is open, as the engine settles an account that is not signed
 * in after its last try. */
async function allSettled() {
  await until(() => attempts().every((row) => row.state === "settled"), ENGINE_MS, "a spend attempt stayed open");
}

/** Reads the spend part of the engine's usage report. Output: the spend view. */
async function spendView() {
  return (await command("desktop_usage_report", { projectId: null, sinceMs: null, offsetMinutes: 0 })).spend;
}

/** Names one option of a choice group. Inputs: the group's label and the option's words. Output:
 * its selector. */
function option(group, words) {
  return `//*[@role="radiogroup"][@aria-label=${JSON.stringify(group)}]//button[normalize-space(.)=${JSON.stringify(words)}]`;
}

/** Picks one option and waits for the engine to keep it. Inputs: the group, the option's words,
 * and the setting field and value the engine should then hold. */
async function choose(group, words, field, value) {
  await click(option(group, words));
  await until(async () => (await spendView()).settings[field] === value, UI_MS, `the engine did not keep ${group}: ${words}`);
}

/** Opens Pools again, so the page reads the usage report now rather than on its minute reread. */
async function reopenPools() {
  await settingsTab("Accounts");
  await settingsTab("Pools");
  await until(() => shown(option(CONFIRM, "Confirm each")), UI_MS, "Pools did not show the spend settings");
}

/** Reads the spend line under Bravo's control in the notice. Output: the line, or "". */
async function spendLine() {
  return (await texts("main .notice .spend-reset-line"))[0] ?? "";
}

describe("Settings, Pools: the two spend settings and the free reset spend on the real desktop app", () => {
  it("When every pool account is limited, Notice (the default) names the accounts with free resets", async () => {
    await finishWelcome("Spend Owner");
    await click("dialog[open] button[aria-label='Close']");
    await until(async () => !(await shown("dialog[open]")), UI_MS, "the Add account dialog did not close");
    for (const name of [ALPHA, BRAVO]) {
      const account = await command("desktop_add_account", { draft: { provider: "chatgpt", method: "subscription", name }, key: null });
      assert.notEqual(account.status, "signed_in", "a new subscription is not signed in, so no spend leaves the app");
      ids[name] = account.id;
    }
    await settingsTab("Pools");
    await click("button=New pool");
    await type("#pool-name", POOL);
    await click(`[data-control="add:${ids[ALPHA]}"]`);
    await until(() => says("main .pool-saved", `${POOL} created.`), UI_MS, "the pool was not created");
    await click(`[data-control="add:${ids[BRAVO]}"]`);
    await until(async () => (await command("desktop_pool_summaries", {})).find((pool) => pool.name === POOL)?.members.length === 2, UI_MS, "Bravo did not join the pool");
    await click("button=Done");
    await seedLimited();
    await until(async () => (await spendView()).limited_pools.length === 1, ENGINE_MS, "the engine did not find the limited pool");
    assert.deepEqual((await spendView()).limited_pools[0].with_resets, [ids[BRAVO]]);
    await reopenPools();
    assert.equal(await (await $(option(LIMITED, "Notice"))).getAttribute("aria-checked"), "true");
    await until(async () => (await texts("main .notice .grow")).includes(NOTICE), UI_MS, "the notice did not name Bravo");
    assert.equal((await texts("main .notice .spend-reset button")).length, 1, "one Spend control, for the account with a free reset");
    assert.deepEqual(attempts(), [], "the notice spends nothing");
    await shot("spend-notice");
  });

  it("Spend confirmation, Confirm each (the default), asks first and then queues one spend", async () => {
    assert.equal(await (await $(option(CONFIRM, "Confirm each"))).getAttribute("aria-checked"), "true");
    await click(`main .notice .spend-reset button`);
    await until(() => says("dialog[open] h2", `${SPEND} on ${BRAVO}?`), UI_MS, "the confirm did not open");
    assert.deepEqual(attempts(), [], "nothing is asked before the confirm");
    await shot("spend-confirm");
    await click('//dialog[@open]//button[normalize-space(.)="Use free reset"]');
    await until(async () => !(await shown("dialog[open]")), UI_MS, "the confirm did not close");
    await until(async () => [QUEUED, NOT_SIGNED_IN].includes(await spendLine()), UI_MS, "the spend line did not show");
    assert.deepEqual(attempts().map((row) => [row.account, row.origin]), [[ids[BRAVO], "owner"]]);
    await allSettled();
    assert.equal(attempts()[0].outcome, "usage_spend_not_signed_in");
  });

  it("Spend confirmation, One click, queues the spend with no confirm", async () => {
    await choose(CONFIRM, "One click", "confirm", "one_click");
    await until(async () => await spendLine() === NOT_SIGNED_IN, UI_MS, "the last outcome did not show");
    await click(`main .notice .spend-reset button`);
    await until(() => attempts().length === 2, UI_MS, "one click did not queue a spend");
    assert.equal(await shown("dialog[open]"), false, "no confirm on one click");
    assert.deepEqual(attempts().map((row) => [row.account, row.origin]), [[ids[BRAVO], "owner"], [ids[BRAVO], "owner"]]);
    await allSettled();
  });

  it("Spend confirmation, Off, shows no Spend and the engine refuses one", async () => {
    await choose(CONFIRM, "Off", "confirm", "off");
    await until(async () => (await texts("main .set-help")).includes(OFF_HELP), UI_MS, "the Off help line did not show");
    await until(async () => !(await shown("main .spend-reset")), UI_MS, "a Spend control stayed while spending is off");
    await assert.rejects(command("desktop_usage_spend", { account: ids[BRAVO] }), /usage_spend_off/);
    assert.equal(attempts().length, 2, "nothing more was queued");
    await shot("spend-off");
  });

  it("When every pool account is limited, No hint shows no notice", async () => {
    await choose(LIMITED, "No hint", "all_limited", "no_hint");
    await until(async () => !(await texts("main .notice .grow")).includes(NOTICE), UI_MS, "the notice stayed on No hint");
    assert.deepEqual((await spendView()).limited_pools, []);
    assert.equal(attempts().length, 2, "no hint spends nothing");
  });

  it("When every pool account is limited, Spend on its own uses one free reset once, with spending on", async () => {
    await choose(LIMITED, "Spend on its own", "all_limited", "auto_spend");
    await until(async () => (await texts("main .set-help")).includes("Spending is off, so CrazyAgent never uses a free reset on its own."), UI_MS, "auto-spend did not say spending is off");
    await new Promise((done) => setTimeout(done, 12000));
    assert.equal(attempts().length, 2, "no auto-spend while spending is off");
    await choose(CONFIRM, "One click", "confirm", "one_click");
    await seedLimited();
    await until(() => attempts().some((row) => row.origin === "auto"), ENGINE_MS, "auto-spend did not queue a spend");
    const auto = attempts().filter((row) => row.origin === "auto");
    assert.deepEqual(auto.map((row) => row.account), [ids[BRAVO]]);
    await allSettled();
    await new Promise((done) => setTimeout(done, 12000));
    assert.equal(attempts().filter((row) => row.origin === "auto").length, 1, "at most one auto-spend per limit window");
    await shot("spend-auto");
  });
});
