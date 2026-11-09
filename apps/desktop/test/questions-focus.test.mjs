import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const FIXTURE = new URL("./fixtures/questions-focus.mjs", import.meta.url).pathname;

async function type(send, text) {
  for (const key of text) {
    await send("Input.dispatchKeyEvent", { type: "keyDown", key, text: key });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key });
  }
}

test("with the inbox already read when the drawer opens, the next poll leaves the keyboard in the answer box", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(true, "questions")`);
    const opened = await evaluate("Fixture.where()");
    assert.equal(await evaluate("Fixture.openAnswer()"), true, "the answer box takes the focus");
    await type(send, "ab");
    await evaluate("Fixture.reread()");
    const after = await evaluate("Fixture.where()");
    await type(send, "c");
    return { opened, after, typed: await evaluate("Fixture.typed()") };
  });
  assert.equal(seen.opened.tag, "li", "the drawer opens on the first question");
  assert.match(seen.opened.row, /Builder 3/);
  assert.equal(seen.after.tag, "textarea", "the poll does not pull the keyboard off the answer box");
  assert.equal(seen.typed, "abc", "every key reaches the answer box");
});

test("opened while the inbox loads, the keyboard starts on the first question once it is read, and only then", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(false, "questions")`);
    const loading = await evaluate("Fixture.where()");
    await evaluate("Fixture.reread()");
    const read = await evaluate("Fixture.where()");
    assert.equal(await evaluate("Fixture.openAnswer()"), true, "the answer box takes the focus");
    await evaluate("Fixture.reread()");
    return { loading, read, after: await evaluate("Fixture.where()") };
  });
  assert.notEqual(seen.loading.tag, "li", "no row exists yet");
  assert.equal(seen.read.tag, "li", "the row takes the focus once it is read");
  assert.match(seen.read.row, /Builder 3/);
  assert.equal(seen.after.tag, "textarea", "it takes the focus once only");
});

test("a toast about an agent waiting in its terminal starts the keyboard on that agent, and a poll leaves it there", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(true, "waiting")`);
    const opened = await evaluate("Fixture.where()");
    await evaluate("Fixture.reread()");
    return { opened, after: await evaluate("Fixture.where()") };
  });
  assert.equal(seen.opened.tag, "li");
  assert.match(seen.opened.row, /Tester 4/);
  assert.deepEqual(seen.after, seen.opened);
});
