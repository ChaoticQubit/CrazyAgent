import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const { UNWATCHED_MS, changed, onChanged, startWatch } = await import("../src/engine-changes.ts");

test("v2.5 S3c: a change reaches each listener of a topic it names once, unknown reaches every listener once, and a stopped one hears nothing", () => {
  const heard = [];
  const stopTeam = onChanged(["team"], () => heard.push("team"));
  const stopBoth = onChanged(["ide", "team"], () => heard.push("both"));
  const stopProjects = onChanged(["projects"], () => heard.push("projects"));
  changed({ topics: ["team"] });
  assert.deepEqual(heard.splice(0), ["team", "both"]);
  changed({ topics: ["ide", "team"] });
  assert.deepEqual(heard.splice(0), ["team", "both"], "a listener of two named topics is called once");
  changed({ topics: ["settings"] });
  assert.deepEqual(heard.splice(0), [], "no listener of settings");
  changed({ topics: ["unknown"] });
  assert.deepEqual(heard.splice(0).sort(), ["both", "projects", "team"]);
  stopBoth();
  changed({ topics: ["projects", "unknown"] });
  assert.deepEqual(heard.splice(0).sort(), ["projects", "team"]);
  stopTeam();
  stopProjects();
  changed({ topics: ["unknown"] });
  assert.deepEqual(heard, []);
  assert.throws(() => onChanged([], () => {}), /needs a topic/);
});

test("v2.5 S3c: the page watches once, and a page that cannot watch hears unknown every 5 s instead", async () => {
  const before = globalThis.window;
  const asked = [];
  const timers = [];
  globalThis.window = {
    __TAURI_INTERNALS__: {
      async invoke(command) {
        asked.push(command);
        throw `Command ${command} not found`;
      },
      transformCallback: () => 1,
    },
    setInterval(work, ms) {
      timers.push([work, ms]);
      return timers.length;
    },
  };
  try {
    const first = startWatch();
    assert.equal(startWatch(), first, "one watch for the page");
    await first;
    assert.deepEqual(asked, ["desktop_watch"]);
    assert.deepEqual(timers.map(([, ms]) => ms), [UNWATCHED_MS]);
    assert.equal(UNWATCHED_MS, 5_000);
    let heard = 0;
    const stop = onChanged(["ide"], () => (heard += 1));
    timers[0][0]();
    assert.equal(heard, 1, "the poll stands in as an unknown change");
    stop();
  } finally {
    globalThis.window = before;
  }
});

test("v2.5 S3c: the IDE focus and the team alerts are read on the engine's watch, with no poll left", () => {
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  const hook = (name) => app.slice(app.indexOf(`function ${name}(`), app.indexOf("\n}\n", app.indexOf(`function ${name}(`)));
  assert.match(hook("useIdeFocus"), /const stopHearing = onChanged\(\["ide"\], \(\) => void read\(\)\);\n {4}void startWatch\(\)\.then\(\(\) => live && read\(\)\);/);
  assert.match(hook("useTeamAlerts"), /const stopHearing = onChanged\(\["team"\], \(\) => void read\(\)\);\n {4}void startWatch\(\)\.then\(\(\) => live && read\(\)\);/);
  for (const name of ["useIdeFocus", "useTeamAlerts"]) assert.doesNotMatch(hook(name), /setTimeout|setInterval/, name);
  assert.doesNotMatch(app, /IDE_FOCUS_POLL_MS|ALERT_READ_MS/);
});
