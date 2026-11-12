import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const view = await import("../src/screens/team/leftovers.tsx");
const controls = await import("../src/screens/team/controls.tsx");

const NOW = Date.UTC(2026, 10, 11, 23, 0);
const hold = (extra = {}) => ({
  key: 7, who: "team agent 156", names: "4242 node, 4243 vite", state: "held", since_ms: NOW - 180_000, holds_account: true,
  ...extra,
});
const panel = (holds) => renderToStaticMarkup(createElement(view.LeftoversPanel, {
  projectId: "p-1", status: { leftover_holds: holds }, nowMs: NOW, reload: () => {},
}));

test("the leftover setting offers End them first, then Keep running and Ask me", () => {
  assert.deepEqual(controls.LEFTOVER_OPTIONS.map((option) => option.value), ["end", "keep", "ask"]);
  assert.equal(controls.LEFTOVER_OPTIONS[0].label, "End them");
  assert.ok(controls.SETTING_GROUPS.find((group) => group.id === "limits").rows.includes("Left running after a stop"));
});

test("a held hold offers End and Keep, an ending one only Keep, a kept one only End", () => {
  assert.deepEqual(view.holdButtons({ state: "held" }), { end: true, keep: true });
  assert.deepEqual(view.holdButtons({ state: "ending" }), { end: false, keep: true });
  assert.deepEqual(view.holdButtons({ state: "kept" }), { end: true, keep: false });
});

test("a hold's age reads in whole minutes", () => {
  assert.equal(view.holdAge(NOW - 30_000, NOW), "just now");
  assert.equal(view.holdAge(NOW - 60_000, NOW), "1 minute ago");
  assert.equal(view.holdAge(NOW - 180_000, NOW), "3 minutes ago");
  assert.equal(view.holdAge(NOW + 5_000, NOW), "just now");
});

test("the panel lists each hold with who left it, what runs and its buttons, and nothing when none run", () => {
  assert.equal(panel([]), "");
  assert.equal(renderToStaticMarkup(createElement(view.LeftoversPanel, { projectId: "p-1", status: {}, nowMs: NOW, reload: () => {} })), "");
  const shown = panel([hold(), hold({ key: 8, who: "the master", state: "ending", holds_account: false })]);
  assert.match(shown, /aria-label="Left running"/);
  assert.match(shown, /team agent 156/);
  assert.match(shown, /4242 node, 4243 vite/);
  assert.match(shown, /3 minutes ago, keeping its account/);
  assert.match(shown, /Waiting for you/);
  assert.match(shown, /Ending/);
  assert.equal((shown.match(/>End</g) ?? []).length, 1);
  assert.equal((shown.match(/>Keep</g) ?? []).length, 2);
});
