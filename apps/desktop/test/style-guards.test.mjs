import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { guardCounts, ratchet, scan } from "./fixtures/style-guards.mjs";

const desktop = fileURLToPath(new URL("..", import.meta.url));
const allow = JSON.parse(readFileSync(new URL("./fixtures/style-guards-allow.json", import.meta.url), "utf8"));

test("a planted literal color, accent use, weight 600 and font size are each counted", () => {
  const planted = ".a { color: #fff; background: rgba(0, 0, 0, 0.1); border-color: var(--accent-soft); font-weight: 600; font-size: 12px; }\n.b { font: 600 11px/16px var(--font); }";
  assert.deepEqual(guardCounts(planted), { color: 2, accent: 1, weight600: 2, fontSize: 2 });
  assert.deepEqual(guardCounts(".a { color: var(--text); font-size: var(--font-body); font-weight: var(--weight-ui); }"),
    { color: 0, accent: 0, weight600: 0, fontSize: 0 });
});

test("weight 600 is allowed on h1 and h2 only, inside media rules too", () => {
  assert.equal(guardCounts("h1 { font-weight: 600; }\nh2 { font-weight: 600; }\nh1, h2 { font-weight: 600; }").weight600, 0);
  assert.equal(guardCounts("h3 { font-weight: 600; }").weight600, 1);
  assert.equal(guardCounts("h1, .title { font-weight: 600; }").weight600, 1);
  assert.equal(guardCounts("@media (min-width: 1px) {\n  .x { font-weight: 600; }\n}").weight600, 1);
});

test("the allowlist only shrinks: a new use, a grown file, a smaller count and a stale entry all fail", () => {
  const counts = (n) => ({ color: { "src/a.css": n }, accent: {}, weight600: {}, fontSize: {} });
  const entry = { color: { "src/a.css": 4 } };
  assert.deepEqual(ratchet(counts(4), entry), []);
  assert.match(ratchet(counts(1), {})[0], /^color src\/a\.css: 1, allowed 0/);
  assert.match(ratchet(counts(5), entry)[0], /allowed at most 4; the allowlist only shrinks/);
  assert.match(ratchet(counts(3), entry)[0], /lower its entry from 4 to 3/);
  assert.match(ratchet(counts(0), entry)[0], /0; remove its entry/);
  assert.match(ratchet({ color: {}, accent: {}, weight600: {}, fontSize: {} }, entry)[0], /no such stylesheet/);
  assert.match(ratchet(counts(4), { color: { "src/a.css": 0 } })[0], /not a count above 0/);
});

test("every desktop stylesheet holds the restyle guards at or below its allowlist entry", () => {
  assert.deepEqual(ratchet(scan(desktop), allow), []);
});
