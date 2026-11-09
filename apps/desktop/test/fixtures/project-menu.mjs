import { createElement } from "react";
import { ProjectMenu } from "../../src/screens/project/index.tsx";
import { mountTab } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const APPLIED = { pending: false, outcome: "applied", rejection: null };
let cancels = [];
let notes = [];

/** The project data the menu reads. Input: each team task's status. Output: the data. */
function project(tasks) {
  return {
    id: "alpha", name: "Alpha", research: undefined, navigate() {}, notify: (note) => notes.push(note), reloadTeam() {},
    team: { result: { kind: "ok", value: { paused: false, tasks: tasks.map((status, id) => ({ id, status })) } }, at: 0 },
  };
}

/** Mounts the real project menu over an engine that applies a cancel. Inputs: the scene, with
 * each team task's status, and the theme. Output: nothing. */
export async function mount(scene, theme = "light") {
  cancels = [];
  notes = [];
  const answer = (command, args) => {
    if (command !== "desktop_team_cancel") throw `Command ${command} not found`;
    cancels.push({ id: args.id, task: args.task });
    return APPLIED;
  };
  await mountTab(createElement(ProjectMenu, { data: project(scene.tasks), replaced: [] }), answer, theme);
}

/** What the engine was asked to cancel. Output: the project and task of each call. */
export function cancelled() {
  return [...cancels];
}

/** What the project told the owner. Output: each note in order. */
export function told() {
  return [...notes];
}
