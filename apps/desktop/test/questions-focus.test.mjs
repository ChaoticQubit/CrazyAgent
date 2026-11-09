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

async function clickAt(send, { x, y }) {
  await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
}

test("a waiting agent that appears after the owner started typing leaves the keyboard in the answer box", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(true, "waiting", false)`);
    await clickAt(send, await evaluate(`Fixture.pointOf("answer")`));
    await evaluate("Fixture.frames()");
    await clickAt(send, await evaluate(`Fixture.pointOf("box")`));
    const typing = await evaluate("Fixture.where()");
    await type(send, "ab");
    await evaluate("Fixture.reread(true)");
    const after = await evaluate("Fixture.where()");
    await type(send, "c");
    await evaluate("Fixture.reread(true)");
    return { typing, after, later: await evaluate("Fixture.where()"), typed: await evaluate("Fixture.typed()") };
  });
  assert.equal(seen.typing.tag, "textarea", "the owner's click put the cursor in the answer box");
  assert.equal(seen.after.tag, "textarea", "the new row does not pull the keyboard off the answer box");
  assert.equal(seen.later.tag, "textarea", "nor does the next read");
  assert.equal(seen.typed, "abc", "every key reaches the answer box");
});

test("a waiting agent that appears later takes the keyboard when the owner is not on a field", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(true, "waiting", false)`);
    const opened = await evaluate("Fixture.where()");
    await evaluate("Fixture.reread(true)");
    return { opened, after: await evaluate("Fixture.where()") };
  });
  assert.notEqual(seen.opened.tag, "li", "no waiting row exists yet");
  assert.equal(seen.after.tag, "li", "the row takes the keyboard once it is read");
  assert.match(seen.after.row, /Tester 4/);
});

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
