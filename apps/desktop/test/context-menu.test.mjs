import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/context-menu.mjs", import.meta.url).pathname);

async function rightClick(send, evaluate, id) {
  const { x, y } = await evaluate(`(() => { const box = document.getElementById(${JSON.stringify(id)}).getBoundingClientRect(); return { x: box.x + box.width / 2, y: box.y + box.height / 2 }; })()`);
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "right", buttons: 2, clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "right", buttons: 0, clickCount: 1 });
}

test("a right-click outside editable text gets no web menu, and editable text keeps its native menu", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    await evaluate("Fixture.mount()");
    for (const id of ["plain", "field", "notes", "editable", "tick"]) await rightClick(send, evaluate, id);
    return evaluate("Fixture.seen");
  });
  assert.deepEqual(seen, [
    { id: "plain", prevented: true },
    { id: "field", prevented: false },
    { id: "notes", prevented: false },
    { id: "editable", prevented: false },
    { id: "tick", prevented: true },
  ]);
});
