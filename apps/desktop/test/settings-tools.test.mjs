import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { ToolSwitchesView, toolState, toolsChanged } = await import("../src/screens/settings/tools.tsx");

const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const tool = (patch = {}) => ({ server: "crazyagent-team", server_id: null, tool: "start_work", description: "Start a unit of work.", builtin: true, global: "on", here: null, ...patch });
const mcp = (patch = {}) => tool({ server: "files", server_id: "server-1", tool: "read_file", description: "", builtin: false, ...patch });
const props = (tools, patch = {}) => ({ tools: { kind: "ok", value: tools }, failure: "", busy: null, onToggle: forbidden, onHide: forbidden, onRestore: forbidden, onRetry: forbidden, ...patch });
const render = (tools, patch) => renderToStaticMarkup(createElement(ToolSwitchesView, props(tools, patch)));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ");

test("each tool that is not deleted has one switch, labelled for every project or this project", () => {
  const tools = [tool(), tool({ tool: "recall", description: "Search memories." }), tool({ tool: "merge", global: "hidden" })];
  const global = render(tools);
  assert.equal(global.split('role="switch"').length - 1, 2, global);
  assert.match(text(global), /On for every project/);
  assert.doesNotMatch(text(global), /On in this project/);
  assert.doesNotMatch(global, /tool-crazyagent-team-merge/);
  const here = render(tools, { projectId: "project-harbor" });
  assert.match(text(here), /On in this project/);
  assert.doesNotMatch(text(here), /On for every project/);
});

test("Delete is offered only on a built-in tool's line", () => {
  const markup = render([tool(), mcp()]);
  assert.match(markup, /aria-label="Delete start_work"/);
  assert.doesNotMatch(markup, /aria-label="Delete read_file"/);
});

test("deleted tools are counted with Restore defaults; Restore shows while anything is stored here", () => {
  const hidden = render([tool(), tool({ tool: "merge", global: "hidden" }), tool({ tool: "spawn", global: "hidden" })]);
  assert.match(text(hidden), /2 deleted\. Restore defaults brings them back\./);
  assert.match(text(hidden), /Restore defaults/);
  assert.match(text(render([tool({ global: "hidden" }), tool({ tool: "recall" })])), /1 deleted\. Restore defaults brings it back\./);
  assert.doesNotMatch(text(render([tool(), tool({ tool: "recall" })])), /Restore defaults/);
  assert.match(text(render([tool({ global: "off" })])), /Restore defaults/);
  assert.doesNotMatch(text(render([tool({ global: "off" })], { projectId: "project-harbor" })), /Restore defaults/);
  assert.match(text(render([tool({ here: "on" })], { projectId: "project-harbor" })), /Restore defaults/);
  assert.match(text(render([tool({ global: "hidden" })])), /Every tool here is deleted\./);
  assert.match(text(render([])), /No tools yet/);
});

test("a tool's switch in a project is its own row, else the every-project row", () => {
  assert.equal(toolState(tool({ global: "off", here: "on" }), "project-harbor"), "on");
  assert.equal(toolState(tool({ global: "off", here: null }), "project-harbor"), "off");
  assert.equal(toolState(tool({ global: "off", here: "on" })), "off");
  assert.equal(toolState(tool({ global: "hidden", here: null })), "hidden");
  assert.equal(toolsChanged([tool({ here: "off" })], "project-harbor"), true);
  assert.equal(toolsChanged([tool({ global: "off" })], "project-harbor"), false);
  assert.equal(toolsChanged([tool({ global: "off" })]), true);
  assert.equal(toolsChanged([tool({ here: "off" })]), false);
  assert.equal(toolsChanged([tool()]), false);
});
