import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/changes-card.mjs", import.meta.url).pathname);
const BADGE_LINE = 22;

async function measure() {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mountCards()");
    await evaluate("Fixture.settled()");
    return evaluate("Fixture.cards()");
  });
}

test("the Agent is fixing conflicts with main badge stays on one line in the 280 px Changes card", async () => {
  const cards = await measure();
  assert.equal(cards.list, 280, `the card list is 280 px wide: ${JSON.stringify(cards)}`);
  const fixing = cards.rows.find((row) => row.word === "Agent is fixing conflicts with main");
  assert.ok(fixing, `the fixing row shows its word: ${JSON.stringify(cards)}`);
  for (const row of cards.rows) {
    assert.equal(row.height, BADGE_LINE, `"${row.word}" is one line: ${JSON.stringify(cards)}`);
    assert.ok(row.right <= row.contentRight, `"${row.word}" ends inside its row: ${JSON.stringify(cards)}`);
    assert.equal(row.overflows, false, `"${row.word}" shows its whole word: ${JSON.stringify(cards)}`);
  }
});

test("each Changes card row spans the card with one line between rows", async () => {
  const cards = await measure();
  for (const row of cards.rows) assert.equal(row.rowWidth, cards.inner, `"${row.word}" spans the card: ${JSON.stringify(cards)}`);
  assert.deepEqual(cards.between, [1], `one line between the two rows: ${JSON.stringify(cards)}`);
});
