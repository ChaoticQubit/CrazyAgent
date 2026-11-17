import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const { COALESCE_MS, UNWATCHED_MS, changed, onChanged, startWatch } = await import("../src/engine-changes.ts");

test("v2.5 S3c: a change reaches each listener of a topic it names once, unknown reaches every listener once, and a stopped one hears nothing", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const release = () => t.mock.timers.tick(COALESCE_MS);
  const heard = [];
  const stopTeam = onChanged(["team"], () => heard.push("team"));
  const stopBoth = onChanged(["ide", "team"], () => heard.push("both"));
  const stopProjects = onChanged(["projects"], () => heard.push("projects"));
  changed({ topics: ["team"] });
  assert.deepEqual(heard.splice(0), ["team", "both"]);
  release();
  changed({ topics: ["ide", "team"] });
  assert.deepEqual(heard.splice(0), ["team", "both"], "a listener of two named topics is called once");
  release();
  changed({ topics: ["settings"] });
  assert.deepEqual(heard.splice(0), [], "no listener of settings");
  release();
  changed({ topics: ["unknown"] });
  assert.deepEqual(heard.splice(0).sort(), ["both", "projects", "team"]);
  release();
  stopBoth();
  changed({ topics: ["projects", "unknown"] });
  assert.deepEqual(heard.splice(0).sort(), ["projects", "team"]);
  release();
  stopTeam();
  stopProjects();
  changed({ topics: ["unknown"] });
  assert.deepEqual(heard, []);
  release();
  assert.throws(() => onChanged([], () => {}), /needs a topic/);
});

test("v2.5 S3c (rv-sec2 #337 N1): 100 named commits in a second reload each listener at most once per 250 ms, the last commit included", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const COMMITS = 100;
  const GAP_MS = 10;
  let commits = 0;
  let team = 0;
  let seenAfter = 0;
  let other = 0;
  const stopTeam = onChanged(["team"], () => {
    team += 1;
    seenAfter = commits;
  });
  const stopOther = onChanged(["projects"], () => (other += 1));
  for (let index = 0; index < COMMITS; index += 1) {
    changed({ topics: ["team"] });
    commits += 1;
    t.mock.timers.tick(GAP_MS);
  }
  t.mock.timers.tick(COALESCE_MS);
  assert.ok(team <= 1 + Math.ceil((COMMITS * GAP_MS) / COALESCE_MS), `${team} reloads`);
  assert.equal(team, 5, "the first at once, then one per window that merged any");
  assert.equal(seenAfter, COMMITS, "a reload follows the last commit");
  assert.equal(other, 0);
  changed({ topics: ["projects"] });
  assert.equal(other, 1, "after a quiet window the next change goes out at once");
  t.mock.timers.tick(COALESCE_MS);
  stopTeam();
  stopOther();
});

test("v2.5 S3c: the page watches once, and a page that cannot watch hears unknown every 5 s instead", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const before = globalThis.window;
  const asked = [];
  globalThis.window = {
    __TAURI_INTERNALS__: {
      async invoke(command) {
        asked.push(command);
        throw `Command ${command} not found`;
      },
      transformCallback: () => 1,
    },
  };
  try {
    const first = startWatch();
    assert.equal(startWatch(), first, "one watch for the page");
    await first;
    assert.deepEqual(asked, ["desktop_watch"]);
    assert.equal(UNWATCHED_MS, 5_000);
    let heard = 0;
    const stop = onChanged(["ide"], () => (heard += 1));
    t.mock.timers.tick(UNWATCHED_MS - 1);
    assert.equal(heard, 0);
    t.mock.timers.tick(1);
    assert.equal(heard, 1, "the poll stands in as an unknown change");
    t.mock.timers.tick(COALESCE_MS);
    stop();
  } finally {
    globalThis.window = before;
  }
});

test("v2.5 S3c: the IDE focus and the team alerts are read on the engine's watch, with no poll left", () => {
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  const hook = (name) => app.slice(app.indexOf(`function ${name}(`), app.indexOf("\n}\n", app.indexOf(`function ${name}(`)));
  assert.match(hook("useIdeFocus"), /const stopHearing = onChanged\(\["ide"\], \(\) => void read\(\)\);\n {4}void startWatch\(\)\.then\(\(\) => \{\n {6}if \(live\) void read\(\);\n {4}\}\);/);
  assert.match(hook("useTeamAlerts"), /const stopHearing = onChanged\(\["team"\], \(\) => void read\(\)\);\n {4}void startWatch\(\)\.then\(\(\) => \{\n {6}if \(live\) void read\(\);\n {4}\}\);/);
  for (const name of ["useIdeFocus", "useTeamAlerts"]) assert.doesNotMatch(hook(name), /setTimeout|setInterval/, name);
  assert.doesNotMatch(app, /IDE_FOCUS_POLL_MS|ALERT_READ_MS/);
});
