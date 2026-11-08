import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { wallPages } = await import("../src/screens/terminals/pages.ts");

/** A graph node for the wall's page math. Inputs: the id, the kind and whether it has a
 * terminal. Output: the node. */
function node(id, kind = "sub", terminal = true) {
  return { id, kind, parent: kind === "master" ? null : "master", label: id, harness: "codex", status: null, status_label: null, account: null, terminal: terminal ? `team-${id}` : null };
}

/** A team in graph order with the master third, as a graph may list it. Input: how many
 * agents besides the master. Output: the nodes. */
function team(agents) {
  const subs = Array.from({ length: agents }, (_, index) => node(`a${index + 1}`));
  return [...subs.slice(0, 2), node("m", "master"), ...subs.slice(2)];
}

const ids = (pages) => pages.map((page) => page.map((item) => item.id));

test("13 terminals at 6 per window take three windows of 6, 6 and 1, the master first and the agents in graph order", () => {
  const pages = wallPages(team(12), 6);
  assert.deepEqual(pages.map((page) => page.length), [6, 6, 1]);
  assert.deepEqual(ids(pages).flat(), ["m", ...Array.from({ length: 12 }, (_, index) => `a${index + 1}`)]);
});

test("a 14th terminal fills the last window, and a 19th opens a fourth", () => {
  assert.deepEqual(wallPages(team(13), 6).map((page) => page.length), [6, 6, 2]);
  assert.deepEqual(wallPages(team(18), 6).map((page) => page.length), [6, 6, 6, 1]);
});

test("each choice fills its windows to 4, 6 or 9", () => {
  assert.deepEqual([4, 6, 9].map((per) => wallPages(team(7), per).map((page) => page.length)), [[4, 4], [6, 2], [8]]);
  assert.deepEqual(wallPages(team(15), 9).map((page) => page.length), [9, 7]);
});

test("agents without a terminal take no tile, and a team with no terminal opens no window", () => {
  const nodes = [node("m", "master"), node("a1", "sub", false), node("a2"), node("lead", "lead", false)];
  assert.deepEqual(ids(wallPages(nodes, 4)), [["m", "a2"]]);
  assert.deepEqual(wallPages([node("m", "master", false)], 6), []);
  assert.deepEqual(wallPages([], 6), []);
});

const { readFileSync } = await import("node:fs");
const { createElement } = await import("react");
const { renderToStaticMarkup } = await import("react-dom/server");
const pages = await import("../src/screens/terminals/pages.ts");
const io = await import("../src/screens/team/terminal-io.ts");
const source = (path) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");

test("a wall window reads its project and page from its own address and nothing else", () => {
  assert.deepEqual(pages.wallRoute("#/terminals/larkspur/1"), { project: "larkspur", page: 1 });
  assert.deepEqual(pages.wallRoute("#/terminals/my%20app/8"), { project: "my app", page: 8 });
  for (const bad of ["", "#/projects/larkspur", "#/terminals/larkspur", "#/terminals/larkspur/0", "#/terminals/larkspur/9",
    "#/terminals/larkspur/2x", "#/terminals//1", "#/terminals/a/b/1", "#/terminals/%E0%A4%A/1"]) {
    assert.equal(pages.wallRoute(bad), null, bad);
  }
});

test("the Open all terminals hint names the windows and the terminals in each", () => {
  assert.equal(pages.openAllHint(2, 6), "Opens 2 windows, 6 terminals each.");
  assert.equal(pages.openAllHint(1, 4), "Opens 1 window of up to 4 terminals.");
  assert.equal(pages.openAllHint(0, 9), "No agent has a terminal yet.");
  assert.equal(pages.WALL_WINDOWS_MAX, 8);
});

test("a terminal past the view bound shows the terminal limit reached state", () => {
  assert.match(io.endCopy("terminal_viewers_full"), /^Terminal limit reached\./u);
});

test("a wall window draws its tiles master first with Message and Stop, then free slots up to the setting", async () => {
  const wall = await import("../src/screens/terminals/index.tsx");
  const nodes = [{ ...node("m", "master"), agent: 9 }, { ...node("a1"), agent: 1 }];
  const html = renderToStaticMarkup(createElement(wall.WallGrid, { projectId: "larkspur", tiles: nodes, nodes, perWindow: 4, onStop: () => {} }));
  assert.equal(html.match(/class="tw-tile"/gu)?.length, 2);
  assert.equal(html.match(/Free slot/gu)?.length, 2);
  assert.ok(html.indexOf("m terminal") < html.indexOf("a1 terminal"), "the master's tile comes first");
  assert.equal(html.match(/aria-label="Message [^"]+"/gu)?.length, 2);
  assert.deepEqual(html.match(/aria-label="Stop [^"]+"/gu), ["aria-label=\"Stop a1\""]);
});

test("each wall tile has the panel's head and input: name, harness and state, role, branch and running time, then Type to and Send", async () => {
  const wall = await import("../src/screens/terminals/index.tsx");
  const nodes = [
    { ...node("m", "master"), label: "Master", agent: 9, status: "idle", status_label: "Idle", branch: "main", started_ms: null },
    { ...node("a1"), agent: 1, status: "working", status_label: "Working", branch: "crazyagent/calendar-api", started_ms: Date.now() - 14 * 60_000 - 5_000 },
  ];
  const html = renderToStaticMarkup(createElement(wall.WallGrid, { projectId: "larkspur", tiles: nodes, nodes, perWindow: 4, onStop: () => {} }));
  const tiles = html.split('class="tw-tile"').slice(1);
  assert.equal(tiles.length, 2);
  const [master, sub] = tiles;
  assert.match(master, /<span class="tg-terminal-name">Master · Codex<\/span><span class="badge">(?:(?!<\/span>)[^])*Idle<\/span>/);
  assert.match(sub, /<span class="tg-terminal-name">a1 · Codex<\/span><span class="badge tone-info">(?:(?!<\/span>)[^])*Working<\/span>/);
  assert.match(master, /class="tg-terminal-meta"[^]*>Master<\/span>[^]*<span class="tg-terminal-branch">main<\/span>/);
  assert.match(sub, /class="tg-terminal-meta"[^]*>Sub-agent<\/span>[^]*<span class="tg-terminal-branch">crazyagent\/calendar-api<\/span>/);
  assert.match(sub, /<span class="tg-terminal-fact">(?:(?!<\/span>)[^])*running 14 min<\/span>/);
  assert.doesNotMatch(master, /running \d/, "an idle master's tile says no running time");
  assert.match(master, /placeholder="Type to Master\.\.\."/);
  assert.match(sub, /placeholder="Type to a1\.\.\."/);
  assert.equal(html.match(/<button type="submit"[^>]*>(?:(?!<\/button>)[^])*Send<\/button>/gu)?.length, 2, "a Send per tile");
  assert.doesNotMatch(html, /Click another agent/, "a wall tile never swaps its agent");
  assert.doesNotMatch(html, /aria-label="Close terminal"/, "a wall tile has no Close");
  const classes = [...html.matchAll(/<section class="([^"]*)"/gu)].map((match) => match[1].split(" "));
  assert.equal(classes.length, 2);
  assert.ok(classes.every((names) => !names.includes("tile")), `style.css .tile is a padded card, never a terminal: ${JSON.stringify(classes)}`);
});

test("the wall boots from main.tsx on its own address, Agents carries the button and the Coordination group the setting", () => {
  assert.match(source("main.tsx"), /wallRoute\(window\.location\.hash\)/u);
  assert.match(source("main.tsx"), /<TerminalWall /u);
  assert.match(source("screens/team/index.tsx"), />Open all terminals</u);
  assert.match(source("screens/team/index.tsx"), /openTerminalWindows\(projectId, name, windows, true\)/u);
  assert.match(source("screens/project/agents.tsx"), /<TeamGraphView [^>]*perWindow=/u);
  const controls = source("screens/team/coordination.tsx");
  assert.match(controls, /label="Terminals per window"/u);
  assert.match(controls, /\{ value: "4", label: "4" \}, \{ value: "6", label: "6 \(default\)" \}, \{ value: "9", label: "9" \}/u);
  assert.match(controls, /How many terminals Open all terminals puts in one window\. More agents open more windows\./u);
  const api = source("api.ts");
  for (const command of ["desktop_team_terminals_per_window", "desktop_terminals_per_window", "desktop_open_terminal_windows"]) {
    assert.ok(api.includes(`"${command}"`), command);
  }
});

test("the wall grows itself: each wall window asks the bridge for its project's windows as its team grows, and main's Agents view no longer does", () => {
  const wall = source("screens/terminals/index.tsx");
  assert.match(wall, /extendTerminalWindows\(projectId, pages\.length\)/u);
  assert.match(wall, /\[projectId, extend\]/u);
  const agents = source("screens/team/index.tsx");
  assert.doesNotMatch(agents, /openTerminalWindows\([^)]*false\)/u);
  assert.doesNotMatch(agents, /\bwalled\b/u);
  assert.match(source("api.ts"), /call<void>\("desktop_extend_terminal_windows", \{ id, count \}\)/u);
});

test("a wall tile attaches naming its own project, so the bridge can refuse a terminal of another project", () => {
  assert.match(source("screens/terminals/index.tsx"), /<TerminalPanel node=\{node\} projectId=\{projectId\} tile input="both" /u);
  const panel = source("screens/team/terminal-panel.tsx");
  assert.match(panel, /projectId\?: string;/u);
  assert.match(panel, /attachTerminal\(name, channel, pane, projectId \?\? null\)/u);
  assert.match(source("api.ts"), /call<number>\("desktop_terminal_attach", \{ agent, pane, output, id: project \}\)/u);
});

test("removing a project the engine applied closes that project's wall windows, from both Remove project screens", () => {
  const projects = source("api-projects.ts");
  const remove = projects.slice(projects.indexOf("export async function removeProject("));
  assert.match(remove.slice(0, remove.indexOf("\n}\n")), /receiptState\(result\.value\)\.kind === "applied"\) await closeProjectWalls\(id\)/u);
  assert.match(source("api.ts"), /call<void>\("desktop_close_project_walls", \{ id \}\)/u);
});

test("a native teammate's pane, which shares its lead's terminal, takes no tile of its own, so no tile replaces another", () => {
  const lead = { ...node("lead", "lead"), terminal: "team-4" };
  const mate = { ...node("mate", "teammate"), terminal: "team-4", pane: "%12" };
  const lone = { ...node("lone", "teammate"), terminal: "team-7", pane: "%3" };
  const pages = wallPages([mate, node("m", "master"), lead, lone, { ...node("mate2", "teammate"), terminal: "team-7", pane: "%4" }], 6);
  assert.deepEqual(ids(pages), [["m", "lead", "lone"]]);
  assert.equal(new Set(pages.flat().map((item) => item.terminal)).size, pages.flat().length, "each tile has its own terminal");
});
