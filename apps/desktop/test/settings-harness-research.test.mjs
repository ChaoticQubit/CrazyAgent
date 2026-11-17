import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const view = await import("../src/screens/settings/harnesses.tsx");

const WORDS = "Research jobs only. Edit with AI, project mapping, roadmap status checks and the Overview run on Codex or Claude Code.";
const quiet = () => undefined;
const entry = (id, name, research) => ({ id, name, status: { kind: "ready" }, status_label: "", executable: `/opt/homebrew/bin/${id}`,
  headless: `${id} -p`, tools: { kind: "switchable" }, model_selection: true, account_kinds: ["api_key"], team: { kind: "lead" }, aliases: [],
  terminal: false, master: false, research, sub_agents: "app", account_protocols: ["openai-chat"] });
const row = (harness) => renderToStaticMarkup(createElement(view.HarnessRowView, { entry: harness, versions: null, mode: "off", onCheck: quiet, onUpdate: quiet, onInstall: quiet, onGuide: quiet }))
  .replace(/&#x27;/g, "'");

test("a listed harness that names research says beside it which research it runs and which harnesses run the rest (T19b 19b-4)", () => {
  assert.equal(view.LISTED_RESEARCH_WORDS, WORDS);
  assert.equal(view.researchLine(entry("standin-acp", "Stand-in ACP", true)), WORDS);
  assert.ok(row(entry("standin-acp", "Stand-in ACP", true)).includes(`Research: ${WORDS}`));
});

test("a native harness and a listed one that does not name research show no research line", () => {
  for (const harness of [entry("codex", "Codex", true), entry("claude-code", "Claude Code", true), entry("pi", "Pi", false)]) {
    assert.equal(view.researchLine(harness), null, harness.id);
    assert.ok(!row(harness).includes("Research:"), harness.id);
  }
});
