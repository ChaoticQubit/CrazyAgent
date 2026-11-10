import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { POLL_MS } from "./e2e-harness.mjs";
import { USAGE_MS, handOff, onEngine } from "./codex-task-engine.mjs";

const LABEL = "Estimated cost at API prices";
const AT = new Date().toISOString();
const MAIN = '[aria-label="Main number"] [role="radio"]';
const PRICES = '[aria-label="Prices"] [role="radio"]';

/** Runs a query on the test engine's own store. Inputs: the engine root and a function given
 * the open store. Output: what the function returned. */
function onStore(runtime, use) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    return use(store);
  } finally {
    store.close();
  }
}

/** Writes one synthetic Codex transcript into an agent's home. Inputs: the engine root, the
 * transcript id and its input tokens. Output: the session name the engine files it under. */
function transcript(runtime, id, input) {
  const agent = onStore(runtime, (store) => store.prepare("SELECT id FROM team_agents WHERE harness='codex' ORDER BY id DESC LIMIT 1").get().id);
  const folder = join(runtime, "team", "agents", `agent-${agent}`, "home", "sessions", ...AT.slice(0, 10).split("-"));
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  const usage = { input_tokens: input, cached_input_tokens: 0, output_tokens: 1, reasoning_output_tokens: 0, total_tokens: input + 1 };
  const name = `rollout-${AT.slice(0, 19).replaceAll(":", "-")}-${id}`;
  const lines = [
    { timestamp: AT, type: "session_meta", payload: { id } },
    { timestamp: AT, type: "turn_context", payload: { model: "gpt-5" } },
    { timestamp: AT, type: "event_msg", payload: { type: "token_count", info: { total_token_usage: usage, last_token_usage: usage }, rate_limits: null } },
  ];
  writeFileSync(join(folder, `${name}.jsonl`), `${lines.map((line) => JSON.stringify(line)).join("\n")}\n`, { mode: 0o600 });
  return name;
}

/** Reads a session's counted input tokens. Inputs: the engine root and the session. Output: the count. */
function inputOf(runtime, session) {
  return onStore(runtime, (store) => Number(store.prepare("SELECT COALESCE(SUM(input),0) AS input FROM usage_tokens WHERE session=?").get(session).input));
}

/** Waits for a check to hold. Inputs: what is awaited, for the failure message, and the check.
 * Output: the check's value. */
async function eventually(what, check) {
  const end = Date.now() + USAGE_MS;
  for (;;) {
    const value = await check();
    if (value) return value;
    assert.ok(Date.now() < end, `timed out waiting for ${what}`);
    await pause(POLL_MS);
  }
}

/** Reads each figure on the page. Input: the page. Output: [main number, tooltip] pairs. */
function figures(page) {
  return page.evaluate(`[...document.querySelectorAll(".mx-figure")].map((node) => [node.querySelector("b")?.innerText ?? "", node.title])`);
}

/** Opens Metrics on a priced transcript, then runs the steps. Inputs: the test context, the
 * folder prefix and the steps, given the owner, the page and the engine root. Output: none. */
async function priced(t, prefix, steps) {
  await onEngine(t, prefix, async ({ projects, runtime, me, page }) => {
    assert.equal(await handOff(me, page, projects[0], runtime), "done");
    const session = transcript(runtime, "cb-e", 1000);
    await eventually("the engine counting the transcript", () => inputOf(runtime, session) === 1000);
    await me.click("Metrics", ".app-nav a.nav-item");
    await eventually("a priced figure", async () => (await figures(page)).some(([main]) => /^<?\$/u.test(main) && main !== "$0.00"));
    await steps({ me, page, runtime });
  });
}

const OPTIONS = { timeout: 600000 };

test("CB-E1 Dollars leads and every figure carries the label", OPTIONS, async (t) => {
  await priced(t, "ca-money-1-", async ({ me, page }) => {
    const shown = await figures(page);
    assert.ok(shown.length > 0);
    assert.ok(shown.every(([main]) => /^<?\$/u.test(main)), JSON.stringify(shown));
    assert.ok(shown.every(([, title]) => title.startsWith(LABEL)), JSON.stringify(shown));
    const wrapped = await page.evaluate(`[...document.querySelectorAll(".mx-share .mono")].filter((node) => node.getBoundingClientRect().height >= 2 * parseFloat(getComputedStyle(node).fontSize)).length`);
    assert.equal(wrapped, 0, "every share percent stays on one line");
    const spill = await page.evaluate(`(() => { const wrap = document.querySelector(".mx-table-wrap"); return wrap.scrollWidth - wrap.clientWidth; })()`);
    assert.equal(spill, 0, "the breakdown table fits its panel");
    await eventually("the credit's copyright line", () => page.evaluate(`document.querySelector(".mx-credit pre")?.textContent.includes("Copyright (c) 2026 AgentSeal") ?? false`));
    await me.capture("metrics-money-dollars");
  });
});

test("CB-E2 Tokens leads, dollars stand beside it, and the choice is remembered", OPTIONS, async (t) => {
  await priced(t, "ca-money-2-", async ({ me, page }) => {
    await me.click("Tokens", MAIN);
    const tokensLead = async () => {
      const shown = await figures(page);
      return shown.length > 0 && shown.every(([main, title]) => !main.includes("$") && title.startsWith(LABEL));
    };
    await eventually("tokens leading", tokensLead);
    await page.send("Page.reload", {});
    await eventually("tokens leading after a reload", tokensLead);
    await me.capture("metrics-money-tokens");
  });
});

test("CB-E3 Bundled shows the snapshot's date on every figure", OPTIONS, async (t) => {
  await priced(t, "ca-money-3-", async ({ me, page }) => {
    const shown = await figures(page);
    assert.ok(shown.every(([, title]) => /prices as of \d{4}-\d{2}-\d{2}$/u.test(title)), JSON.stringify(shown));
    assert.equal(await page.evaluate(`document.querySelector(".mx-page").innerText.includes("Prices as of")`), true);
    await me.capture("metrics-money-bundled");
  });
});

test("CB-E4 Off shows no dollar anywhere", OPTIONS, async (t) => {
  await priced(t, "ca-money-4-", async ({ me, page }) => {
    await me.click("Off", PRICES);
    await eventually("no figure in dollars", async () => {
      const shown = await figures(page);
      return shown.length > 0 && shown.every(([main, title]) => !main.includes("$") && title === "");
    });
    assert.doesNotMatch(await page.evaluate(`document.querySelector(".mx-page").innerText`), /\$\d/u);
    await me.capture("metrics-money-off");
  });
});

test("A refused money switch says so on the page", OPTIONS, async (t) => {
  await priced(t, "ca-money-5-", async ({ me, page, runtime }) => {
    onStore(runtime, (store) => store.exec(`CREATE TRIGGER refuse_money_insert BEFORE INSERT ON usage_money BEGIN SELECT RAISE(ABORT, 'refused'); END;
      CREATE TRIGGER refuse_money_update BEFORE UPDATE ON usage_money BEGIN SELECT RAISE(ABORT, 'refused'); END;`));
    await me.click("Tokens", MAIN);
    await eventually("the refusal on the page", () => page.evaluate(`document.querySelector(".mx-page [role=alert]")?.innerText.includes("Usage isn't available") ?? false`));
    const shown = await figures(page);
    assert.ok(shown.every(([main]) => /^<?\$/u.test(main)), JSON.stringify(shown));
    await me.capture("metrics-money-refused");
    onStore(runtime, (store) => store.exec("DROP TRIGGER refuse_money_insert; DROP TRIGGER refuse_money_update;"));
    await me.click("Tokens", MAIN);
    await eventually("the refusal gone after a good switch", () => page.evaluate(`document.querySelector(".mx-page [role=alert]") === null`));
    await eventually("tokens leading after the good switch", async () => (await figures(page)).every(([main]) => !main.includes("$")));
  });
});

/** Writes a synthetic Codex transcript with one user turn that edits. Inputs: the engine root,
 * the transcript id and the time its lines carry. Output: none. */
function editingTurn(runtime, id, at = new Date().toISOString()) {
  const agent = onStore(runtime, (store) => store.prepare("SELECT id FROM team_agents WHERE harness='codex' ORDER BY id DESC LIMIT 1").get().id);
  const folder = join(runtime, "team", "agents", `agent-${agent}`, "home", "sessions", "2026", "11", "09");
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  const usage = { input_tokens: 10, cached_input_tokens: 0, output_tokens: 1, reasoning_output_tokens: 0, total_tokens: 11 };
  const lines = [
    { timestamp: at, type: "session_meta", payload: { id } },
    { timestamp: at, type: "turn_context", payload: { model: "gpt-5" } },
    { timestamp: at, type: "event_msg", payload: { type: "user_message", message: "add a page" } },
    { timestamp: at, type: "response_item", payload: { type: "function_call", name: "apply_patch", arguments: "{}" } },
    { timestamp: at, type: "event_msg", payload: { type: "token_count", info: { total_token_usage: usage, last_token_usage: usage }, rate_limits: null } },
    { timestamp: at, type: "event_msg", payload: { type: "task_complete" } },
  ];
  writeFileSync(join(folder, `rollout-2026-11-09T02-00-00-${id}.jsonl`), `${lines.map((line) => JSON.stringify(line)).join("\n")}\n`, { mode: 0o600 });
}

test("CB-E7 by model, by activity and the first-try rate show on the page", OPTIONS, async (t) => {
  await onEngine(t, "ca-activity-", async ({ projects, runtime, me, page }) => {
    assert.equal(await handOff(me, page, projects[0], runtime), "done");
    editingTurn(runtime, "cb-a");
    await me.click("Metrics", ".app-nav a.nav-item");
    const panelText = () => page.evaluate(`document.querySelector(".mx-page").innerText`);
    await eventually("the activity panel", async () => (await panelText()).includes("By activity"));
    const text = await panelText();
    assert.match(text, /By model/u);
    assert.match(text, /gpt-5/u);
    assert.match(text, /Feature/u);
    assert.match(text, /100% of 1 editing turn worked first try/u);
    assert.match(text, /Tools over time/u);
    assert.match(text, /apply_patch/u);
    await me.capture("metrics-activity");
  });
});
