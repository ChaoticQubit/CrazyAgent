import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const money = await import("../src/screens/metrics/money.ts");

const tokens = { input: 900, output: 100, cache_read: 5000, cache_write: 0 };
const view = (patch = {}) => ({ settings: { main_number: "dollars", prices: "bundled" }, shown: "dollars", as_of: "2026-11-08", cost_nanos: 0, unpriced_tokens: 0, ...patch });

test("the cost label is the owner's exact words", () => {
  assert.equal(money.COST_LABEL, "Estimated cost at API prices");
});

test("dollars read in dollars and cents and a sliver is not zero", () => {
  assert.equal(money.dollarText(1_234_560_000_000), "$1,234.56");
  assert.equal(money.dollarText(0), "$0.00");
  assert.equal(money.dollarText(4_000_000), "<$0.01");
  assert.equal(money.dollarText(5_000_000), "$0.01");
});

test("the main number leads, the other stands beside it, every dollar is labeled", () => {
  const dollars = money.figure(tokens, 5_000_000_000, view());
  assert.deepEqual([dollars.main, dollars.beside, dollars.title], ["$5.00", "1K tokens", "Estimated cost at API prices, prices as of 2026-11-08"]);
  const flipped = money.figure(tokens, 5_000_000_000, view({ shown: "tokens" }));
  assert.deepEqual([flipped.main, flipped.beside, flipped.title], ["1K", "$5.00", "Estimated cost at API prices, prices as of 2026-11-08"]);
});

test("with prices off no dollar is shown and nothing is labeled as one", () => {
  const off = money.figure(tokens, 0, view({ shown: "tokens", as_of: null, settings: { main_number: "dollars", prices: "off" } }));
  assert.deepEqual([off.main, off.beside, off.title], ["1K", "", ""]);
});

test("rendered figures carry the label on every dollar", () => {
  const markup = renderToStaticMarkup(createElement(money.Figure, { tokens, costNanos: 5_000_000_000, money: view() }));
  assert.match(markup, /title="Estimated cost at API prices, prices as of 2026-11-08"/u);
  assert.match(markup, /\$5\.00/u);
});

test("tokens without a price are said, with the words that name them", () => {
  assert.equal(money.unpricedNote(view({ unpriced_tokens: 1200 })), "1.2K tokens have no price, so they are left out of the dollars.");
  assert.equal(money.unpricedNote(view()), "");
});

test("the first-try rate counts only turns that edited", () => {
  assert.equal(money.firstTryText([{ activity: "coding", turns: 4, edit_turns: 4, first_try_turns: 3 }, { activity: "git", turns: 9, edit_turns: 0, first_try_turns: 0 }]), "75% of 4 editing turns worked first try");
  assert.equal(money.firstTryText([{ activity: "git", turns: 9, edit_turns: 0, first_try_turns: 0 }]), "");
});

test("the first-try rate rounds to the nearest percent and says one turn", () => {
  assert.equal(money.firstTryText([{ edit_turns: 3, first_try_turns: 2 }]), "67% of 3 editing turns worked first try");
  assert.equal(money.firstTryText([{ edit_turns: 1, first_try_turns: 1 }]), "100% of 1 editing turn worked first try");
});

test("every activity the engine files has its name", () => {
  assert.deepEqual(Object.keys(money.ACTIVITY_NAMES).sort(), ["brainstorming", "build_deploy", "coding", "conversation", "debugging", "delegation", "exploration", "feature", "general", "git", "planning", "refactoring", "testing"]);
  assert.equal(money.ACTIVITY_NAMES.build_deploy, "Build/deploy");
  assert.equal(money.ACTIVITY_NAMES.coding, "Coding");
});
