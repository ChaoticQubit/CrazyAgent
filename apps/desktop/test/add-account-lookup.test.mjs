import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { tabRunner } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/add-account-lookup.mjs", import.meta.url).pathname);
const onForm = tabRunner(fixture);
const FIRST = "http://127.0.0.1:11434/v1";
const SECOND = "http://127.0.0.1:9999/v1";
const shown = (page) => page.evaluate("Fixture.shown()");
const typeUrl = (page, words) => page.evaluate(`Fixture.typeUrl(${JSON.stringify(words)})`);
const answer = (page, url, reply) => page.evaluate(`Fixture.answer(${JSON.stringify(url)}, ${JSON.stringify(reply)})`);

test("a server that could not be reached is named by the URL looked up, not the URL typed after", async () => {
  const seen = await onForm({}, async (page) => {
    await page.evaluate("Fixture.findModels()");
    assert.equal(await answer(page, FIRST, { code: "account_url_unreachable" }), true);
    await typeUrl(page, SECOND);
    return shown(page);
  });
  assert.equal(seen.alert, `Can't reach the local server at ${FIRST}. Start it, then try again.`);
});

test("a server with no models is named by the URL looked up, not the URL typed after", async () => {
  const seen = await onForm({}, async (page) => {
    await page.evaluate("Fixture.findModels()");
    assert.equal(await answer(page, FIRST, { models: [] }), true);
    await typeUrl(page, SECOND);
    return shown(page);
  });
  assert.equal(seen.empty, `No models found. Pull one on the local server at ${FIRST}, then try again.`);
});

test("a late answer from an earlier lookup never replaces the latest lookup's models or name", async () => {
  const seen = await onForm({}, async (page) => {
    await page.evaluate("Fixture.findModels()");
    await typeUrl(page, SECOND);
    await page.evaluate("Fixture.findModels()");
    assert.equal(await answer(page, SECOND, { code: "account_url_unreachable" }), true);
    assert.equal(await answer(page, FIRST, { models: ["llama3"] }), true);
    return shown(page);
  });
  assert.deepEqual(seen, { alert: `Can't reach the local server at ${SECOND}. Start it, then try again.`, empty: "", choices: [] });
});
