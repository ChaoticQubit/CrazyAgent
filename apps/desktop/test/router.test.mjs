import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { parseRoute, formatRoute, SETTINGS_TABS, PROJECT_TABS, NEW_PROJECT_STEPS } = await import("../src/ui/router.ts");

test("every design route parses to its screen and formats back to the same hash", () => {
  const cases = [
    ["#/welcome", { name: "welcome" }],
    ["#/home", { name: "home" }],
    ["#/projects/new?step=basics", { name: "new-project", step: "basics" }],
    ["#/projects/new?step=team", { name: "new-project", step: "team" }],
    ["#/projects/harbor-notes/overview", { name: "project", id: "harbor-notes", tab: "overview" }],
    ["#/projects/harbor-notes/context", { name: "project", id: "harbor-notes", tab: "context" }],
    ["#/settings/accounts", { name: "settings", tab: "accounts" }],
    ["#/settings/accounts?action=add", { name: "settings", tab: "accounts", action: "add" }],
    ["#/settings/pools?action=new", { name: "settings", tab: "pools", action: "new" }],
    ["#/settings/profile", { name: "settings", tab: "profile" }],
  ];
  for (const [hash, route] of cases) {
    assert.deepEqual(parseRoute(hash), route, hash);
    assert.equal(formatRoute(route), hash);
  }
  for (const tab of SETTINGS_TABS) assert.deepEqual(parseRoute(formatRoute({ name: "settings", tab })), { name: "settings", tab });
  for (const tab of PROJECT_TABS) assert.deepEqual(parseRoute(formatRoute({ name: "project", id: "p 1/é", tab })), { name: "project", id: "p 1/é", tab });
  for (const step of NEW_PROJECT_STEPS) assert.deepEqual(parseRoute(formatRoute({ name: "new-project", step })), { name: "new-project", step });
  assert.deepEqual(SETTINGS_TABS, ["accounts", "pools", "harnesses", "plugins", "memory", "profile"]);
  assert.deepEqual(PROJECT_TABS, ["overview", "conversation", "agents", "context", "settings"]);
  assert.deepEqual(NEW_PROJECT_STEPS, ["basics", "documents", "work", "pool", "team"]);
});

test("missing parts fall back to the first tab or step, unknown routes fall back to Home", () => {
  assert.deepEqual(parseRoute(""), { name: "home" });
  assert.deepEqual(parseRoute("#"), { name: "home" });
  assert.deepEqual(parseRoute("#/"), { name: "home" });
  assert.deepEqual(parseRoute("#/settings"), { name: "settings", tab: "accounts" });
  assert.deepEqual(parseRoute("#/settings/nope"), { name: "settings", tab: "accounts" });
  assert.deepEqual(parseRoute("#/settings/pools?action=drop"), { name: "settings", tab: "pools" });
  assert.deepEqual(parseRoute("#/projects/new"), { name: "new-project", step: "basics" });
  assert.deepEqual(parseRoute("#/projects/new?step=launch"), { name: "new-project", step: "basics" });
  assert.deepEqual(parseRoute("#/projects/abc"), { name: "project", id: "abc", tab: "overview" });
  assert.deepEqual(parseRoute("#/projects/abc/secret"), { name: "project", id: "abc", tab: "overview" });
  for (const hash of ["#/projects/", "#/projects/%E0%A4%A", "#/projects/" + "x".repeat(257) + "/overview", "#/approvals", "#/javascript:alert(1)"]) {
    assert.deepEqual(parseRoute(hash), { name: "home" }, hash);
  }
});
