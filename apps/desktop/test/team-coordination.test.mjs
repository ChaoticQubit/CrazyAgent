import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const coord = await import("../src/screens/team/coordination.tsx");

test("an engine that sends no coordination shows the plan defaults, native only for Claude Code", () => {
  const view = coord.coordination({ coordination: undefined });
  assert.deepEqual(view.settings, coord.COORD_DEFAULTS);
  assert.deepEqual(view.harnesses, [
    { harness: "claude-code", mode: "app_spawned", delivery: "auto", native: true },
    { harness: "codex", mode: "app_spawned", delivery: "auto", native: false },
    { harness: "rcli", mode: "app_spawned", delivery: "auto", native: false },
  ]);
  const sent = { settings: { ...coord.COORD_DEFAULTS, nudges: false }, harnesses: [{ harness: "codex", mode: "app_spawned", delivery: "typing_only", native: false }] };
  const merged = coord.coordination({ coordination: sent });
  assert.equal(merged.settings.nudges, false);
  assert.equal(merged.harnesses[1].delivery, "typing_only");
  assert.equal(merged.harnesses[0].harness, "claude-code");
});

test("the desktop defaults are the engine's counted tools", () => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/coord/settings.rs", import.meta.url), "utf8");
  const block = source.slice(source.indexOf("pub const COUNTED_TOOLS_DEFAULT"), source.indexOf("];", source.indexOf("pub const COUNTED_TOOLS_DEFAULT")));
  const engine = [...block.matchAll(/"([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(coord.COORD_DEFAULTS.counted_tools, engine);
});

test("Native is offered only where the harness kind has native teammates", () => {
  assert.deepEqual(coord.modeOptions(false).map((option) => option.value), ["app_spawned"]);
  assert.deepEqual(coord.modeOptions(true).map((option) => option.value), ["app_spawned", "native"]);
  assert.deepEqual(coord.DELIVERY_OPTIONS.map((option) => option.value), ["auto", "native_only", "typing_only"]);
});

test("counted tools are read with the engine's rule: once each, at most 32, wildcards allowed", () => {
  assert.deepEqual(coord.countedToolsInput("start_work, ctx_*\nstart_work,, graphify"), { kind: "ok", tools: ["start_work", "ctx_*", "graphify"] });
  const many = Array.from({ length: 33 }, (_, index) => `tool_${index}`).join(",");
  assert.deepEqual(coord.countedToolsInput(many), { kind: "invalid", reason: "Count at most 32 tools." });
  const thirtyTwo = Array.from({ length: 32 }, (_, index) => `tool_${index}`).join(",");
  assert.equal(coord.countedToolsInput(thirtyTwo).kind, "ok");
  assert.deepEqual(coord.countedToolsInput("bad name!"), { kind: "invalid", reason: "\"bad name!\" isn't a tool name. Use letters, digits, _, - or * only." });
  assert.equal(coord.countedToolsInput("x".repeat(65)).kind, "invalid");
  assert.deepEqual(coord.countedToolsInput(""), { kind: "ok", tools: [] });
});
