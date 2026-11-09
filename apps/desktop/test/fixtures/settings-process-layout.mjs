import { createElement } from "react";
import { Settings } from "../../src/screens/settings/index.tsx";
import { mountTab, sceneRead } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const FAILURE = { code: "engine_failed" };
const SKILL = { id: "skill-0123456789abcdef01234567", name: "tdd", description: "Test first.", line: "Test first." };
const TEXTS = { ok: { text: "Use tdd for every change.\n", version: 1 }, empty: { text: "", version: 1 }, large: { text: "x".repeat(65537), version: 1 } };
const SAVES = {
  ok: { kind: "saved", version: 2, size: { bytes: 26, claude_bytes_max: 1000 } },
  stale: { kind: "stale", version: 3, current: { text: "Someone else saved this.\n", version: 3 }, writer: "owner" },
};

/** Answers one engine command for a scene. Inputs: the scene and the command. Output: the
 * answer, or a failure thrown. */
function answer(scene, command) {
  if (command === "desktop_process_text") return sceneRead(scene.saved ?? "ok", TEXTS, FAILURE, command);
  if (command === "desktop_plugin_skills") return [SKILL];
  if (command === "desktop_default_process") return sceneRead(scene.shipped ?? "ok", { ok: "The shipped process.\n" }, FAILURE, command);
  if (command === "desktop_import_instructions_file") return sceneRead(scene.file ?? "ok", { ok: "From my file.\n", closed: null }, FAILURE, command);
  if (command === "desktop_save_process_text") return sceneRead(scene.save ?? "ok", SAVES, FAILURE, command);
  throw `Command ${command} not found`;
}

/** Mounts the Process tab on the Settings page against a fake engine. Inputs: the scene, naming
 * each read as ok, empty, large, closed, stale, error, missing or pending, and the theme.
 * Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  await mountTab(createElement(Settings, { tab: "process" }), (command) => answer(scene, command), theme);
}
