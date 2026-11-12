import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

const controls = await import("../src/screens/team/controls.tsx");

test("End a turn after done offers Off, 2 min, 10 min and 1 h and shows the engine's choice (F15)", () => {
  assert.deepEqual(controls.DONE_QUIET_OPTIONS.map((option) => [option.value, option.label]), [["off", "Off"], ["120", "2 min"], ["600", "10 min"], ["3600", "1 h"]]);
  assert.equal(controls.doneQuietValue({ done_quiet_secs: null }), "off");
  assert.equal(controls.doneQuietValue({ done_quiet_secs: undefined }), "600", "an older engine reads as the 10 min default");
  for (const secs of [120, 600, 3600]) assert.equal(controls.doneQuietValue({ done_quiet_secs: secs }), String(secs));
  for (const option of controls.DONE_QUIET_OPTIONS) {
    const secs = option.value === "off" ? null : Number(option.value);
    assert.equal(controls.doneQuietValue({ done_quiet_secs: secs }), option.value, "each option reads back as itself");
  }
});
