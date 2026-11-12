const assert = require("node:assert/strict");
const test = require("node:test");
const { BREADS, breadNames } = require("../app.js");

test("today's breads are listed by name, with blank names left out", () => {
  assert.deepEqual(breadNames([" Rye ", "", "Baguette"]), ["Rye", "Baguette"]);
  assert.ok(BREADS.length > 0);
});
