import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { bundle, withPage } from "./browser.mjs";
import "./load-source.mjs";

const { Sidebar } = await import("../src/ui/shell.tsx");
const wake = await import("../src/screens/team/wake.ts");
const sidebarPage = await bundle(new URL("./fixtures/sidebar-live.mjs", import.meta.url).pathname);
const alertsPage = await bundle(new URL("./fixtures/team-alerts-read.mjs", import.meta.url).pathname);

const LONG_NAME = "Maximiliana Featherstonehaugh-Cholmondeley";
const SHIPPED = { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 };
const NAME_WIDTH_MIN_PX = 24;
const TEXT_CONTRAST_MIN = 4.5;
const MARK_CONTRAST_MIN = 3;

const sidebar = (projects) => renderToStaticMarkup(createElement(Sidebar, { route: { name: "home" }, projects, profileName: "Mira", engine: "running", onRetry: () => {} }));
const list = (markup) => markup.slice(markup.indexOf('<div class="nav-group">'), markup.indexOf("</nav>"));
const events = (working, projects = []) => ({ rule: null, rule_version: 0, effective: SHIPPED, notifications: [], unseen: 0, projects, working });
const count = (asked, command) => asked.filter((item) => item === command).length;

/** Runs a check on a mounted sidebar. Inputs: the mount arguments as source text, the window
 * width and the check. Output: the check's value. */
function onSidebar(mount, width, check) {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width, height: 800, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(sidebarPage);
    await page.evaluate(`Fixture.mount(${mount})`);
    return check(page);
  });
}

test("the engine's working list becomes one count per project, without zero or an older reply's missing list", () => {
  const working = [{ project_id: "a", agents: 2 }, { project_id: "b", agents: 0 }, { project_id: "c", agents: 1 }];
  assert.deepEqual([...wake.workingByProject({ working })], [["a", 2], ["c", 1]]);
  assert.deepEqual([...wake.workingByProject({})], []);
});

test("a project with agents working draws the dot and the count and gives a screen reader the words", () => {
  const two = list(sidebar([{ id: "p", name: "Larkspur", working: 2 }]));
  assert.match(two, /<span class="nav-live-dot" aria-hidden="true"><\/span><span class="name">Larkspur<\/span>/u);
  assert.match(two, /<span class="nav-live" title="2 agents working"><span aria-hidden="true">2<\/span><span class="visually-hidden">2 agents working<\/span><\/span>/u);
  assert.doesNotMatch(two, /<svg/u, "the dot stands where the folder stands");
  assert.match(list(sidebar([{ id: "p", name: "Larkspur", working: 1 }])), /title="1 agent working"/u);
});

test("a project with no agents working draws no dot and no words and keeps its folder and unseen count", () => {
  for (const working of [undefined, 0]) {
    const row = list(sidebar([{ id: "p", name: "Harbor", flag: "3", working }]));
    assert.doesNotMatch(row, /nav-live|agents? working/u);
    assert.match(row, /<svg/u);
    assert.match(row, /<span class="nav-flag">3<\/span>/u);
    assert.doesNotMatch(row, />0</u);
  }
});

test("a project with agents working keeps its unseen count after the running count", () => {
  const row = list(sidebar([{ id: "p", name: "Larkspur", flag: "3", working: 2 }]));
  assert.ok(row.indexOf("nav-live\"") !== -1 && row.indexOf("nav-live\"") < row.indexOf("nav-flag"), row);
});

for (const theme of ["light", "dark"]) {
  test(`at 320 px in the ${theme} theme a long name keeps its width beside the dot, the counts and the idle rows`, async () => {
    const projects = [
      { id: "a", name: LONG_NAME, flag: "30", working: 12 },
      { id: "b", name: LONG_NAME, flag: "3" },
      { id: "c", name: LONG_NAME, flag: "300", working: 1234 },
    ];
    const shown = await onSidebar(`${JSON.stringify(projects)}, ${JSON.stringify(theme)}`, 320, async ({ evaluate }) => ({
      rows: await evaluate("Fixture.rows()"), overflow: await evaluate("Fixture.listOverflow()"),
    }));
    assert.equal(shown.overflow, 0, JSON.stringify(shown));
    assert.equal(shown.rows[0].dotWidth, 8, JSON.stringify(shown));
    assert.equal(shown.rows[0].dotHeight, 8, JSON.stringify(shown));
    assert.equal(shown.rows[0].nameLeft, shown.rows[1].nameLeft, `names line up with and without a dot: ${JSON.stringify(shown)}`);
    for (const row of shown.rows) {
      assert.ok(row.nameWidth >= NAME_WIDTH_MIN_PX, `the name keeps its width: ${JSON.stringify(row)}`);
      assert.ok(row.flagRight <= row.rowRight, `the unseen count stays in its row: ${JSON.stringify(row)}`);
    }
    assert.ok(shown.rows[0].countRight !== null && shown.rows[0].countRight <= shown.rows[0].flagRight, JSON.stringify(shown));
    assert.ok(shown.rows[2].countRight !== null && shown.rows[2].countRight <= shown.rows[2].flagRight, JSON.stringify(shown));
  });

  test(`in the ${theme} theme the count reads at 4.5 to 1 and the dot shows at 3 to 1 on a plain, hovered and current row`, async () => {
    const projects = JSON.stringify([{ id: "a", name: "Larkspur", working: 2 }]);
    const read = async (evaluate) => {
      const parts = await evaluate("Fixture.firstRowParts()");
      assert.notEqual(parts.dot, null, "the row draws the dot");
      assert.equal(parts.count, true, "the row draws the count");
      return { text: await evaluate('Fixture.contrast(".nav-group .nav-live")'), mark: await evaluate('Fixture.markContrast(".nav-group .nav-live-dot")') };
    };
    const measured = await onSidebar(`${projects}, ${JSON.stringify(theme)}`, 1180, async ({ evaluate }) => {
      const plain = await read(evaluate);
      await evaluate("Fixture.hoverFirstRow()");
      const hovered = await read(evaluate);
      await evaluate(`Fixture.mount(${projects}, ${JSON.stringify(theme)}, { name: "project", id: "a", tab: "overview" })`);
      return { plain, hovered, current: await read(evaluate) };
    });
    for (const [state, shown] of Object.entries(measured)) {
      assert.ok(shown.text >= TEXT_CONTRAST_MIN, `${state} count ${shown.text}`);
      assert.ok(shown.mark >= MARK_CONTRAST_MIN, `${state} dot ${shown.mark}`);
    }
  });
}

test("a screen reader hears the project and how many agents are working, as one sentence with spaces", async () => {
  const projects = JSON.stringify([{ id: "a", name: "Larkspur", flag: "3", working: 2 }, { id: "b", name: "Harbor" }]);
  const names = await onSidebar(`${projects}, "light"`, 1180, async ({ send }) => {
    await send("Accessibility.enable", {});
    const { nodes } = await send("Accessibility.getFullAXTree", {});
    return nodes.filter((node) => node.role?.value === "link").map((node) => node.name?.value);
  });
  assert.ok(names.includes("Larkspur 2 agents working 3 unseen alerts"), JSON.stringify(names));
  assert.ok(names.includes("Harbor"), JSON.stringify(names));
});

test("a screen reader hears what the unseen count counts, for one alert and for several", async () => {
  const projects = JSON.stringify([{ id: "a", name: "Cedar", flag: "1" }, { id: "b", name: "Harbor", flag: "12" }]);
  const names = await onSidebar(`${projects}, "light"`, 1180, async ({ send }) => {
    await send("Accessibility.enable", {});
    const { nodes } = await send("Accessibility.getFullAXTree", {});
    return nodes.filter((node) => node.role?.value === "link").map((node) => node.name?.value);
  });
  assert.ok(names.includes("Cedar 1 unseen alert"), JSON.stringify(names));
  assert.ok(names.includes("Harbor 12 unseen alerts"), JSON.stringify(names));
});

test("one events read per look gives every project its count, and no read is added for each project", async () => {
  await withPage(async ({ evaluate }) => {
    await evaluate(alertsPage);
    const working = [{ project_id: "a", agents: 2 }, { project_id: "b", agents: 1 }, { project_id: "c", agents: 5 }];
    const first = await evaluate(`Fixture.start(${JSON.stringify(events(working))})`);
    assert.deepEqual(first.shown.working, [["a", 2], ["b", 1], ["c", 5]]);
    assert.equal(count(first.asked, "desktop_team_events"), 1);
    assert.equal(first.armed, true, "the same timer carries the next look");
    const second = await evaluate(`Fixture.look(${JSON.stringify(events([{ project_id: "a", agents: 1 }]))})`);
    assert.deepEqual(second.shown.working, [["a", 1]]);
    assert.equal(count(second.asked, "desktop_team_events"), 2);
    const appWide = ["desktop_question_counts", "desktop_system_notifications", "desktop_team_attention", "desktop_team_events"];
    assert.deepEqual([...new Set(second.asked)].filter((item) => !appWide.includes(item)), [], "every read is app-wide, none is per project");
  });
});

test("a failed read or a stopped engine claims no agent is working and keeps the unseen count", async () => {
  await withPage(async ({ evaluate }) => {
    await evaluate(alertsPage);
    const body = events([{ project_id: "a", agents: 2 }], [{ project_id: "a", unseen: 4 }]);
    const first = await evaluate(`Fixture.start(${JSON.stringify(body)})`);
    assert.deepEqual(first.shown, { alerts: [["a", 4]], working: [["a", 2]] });
    const failed = await evaluate("Fixture.look(null)");
    assert.deepEqual(failed.shown, { alerts: [["a", 4]], working: [] }, "a failed read leaves no stale dot");
    const back = await evaluate(`Fixture.look(${JSON.stringify(body)})`);
    assert.deepEqual(back.shown.working, [["a", 2]]);
    const stopped = await evaluate("Fixture.engineStops()");
    assert.deepEqual(stopped.shown, { alerts: [["a", 4]], working: [] }, "a stopped engine has nothing working");
  });
});
