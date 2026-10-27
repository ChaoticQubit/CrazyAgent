import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/secret-rows.mjs", import.meta.url).pathname);
const tick = "const tick = () => new Promise((done) => setTimeout(done, 0));";
const addRow = `(async () => { ${tick}
  [...document.querySelectorAll("button")].find((button) => button.textContent === "Add secret").click();
  await tick();
})()`;
const read = `(async () => { ${tick} await tick(); return Fixture.secrets(); })()`;

test("removing a secret row in Add MCP server keeps every other typed value with its own name", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    const type = async (row, field, text) => {
      await evaluate(`document.querySelectorAll(".row")[${row}].querySelector("${field}").focus()`);
      await send("Input.insertText", { text });
    };
    await evaluate(fixture);
    await evaluate("Fixture.mount()");
    for (const [row, name, value] of [[0, "A", "a"], [1, "B", "b"], [2, "C", "c"]]) {
      await evaluate(addRow);
      await type(row, "input:not([type=password])", name);
      await type(row, "input[type=password]", value);
    }
    const typed = await evaluate(read);
    await evaluate(`(async () => { ${tick}
      document.querySelector(".row [aria-label='Remove secret']").click();
      await tick();
    })()`);
    const removed = await evaluate(read);
    const masked = await evaluate(`[...document.querySelectorAll("input[type=password]")].map((input) => input.value)`);
    await evaluate(addRow);
    await type(2, "input:not([type=password])", "D");
    await type(2, "input[type=password]", "d");
    const added = await evaluate(read);
    return { typed, removed, masked, added };
  });
  assert.deepEqual(seen.typed, [{ name: "A", value: "a" }, { name: "B", value: "b" }, { name: "C", value: "c" }]);
  assert.deepEqual(seen.removed, [{ name: "B", value: "b" }, { name: "C", value: "c" }]);
  assert.deepEqual(seen.masked, ["b", "c"]);
  assert.deepEqual(seen.added, [{ name: "B", value: "b" }, { name: "C", value: "c" }, { name: "D", value: "d" }]);
});
