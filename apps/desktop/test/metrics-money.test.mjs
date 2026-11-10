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
