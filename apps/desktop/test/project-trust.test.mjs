import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const trust = await import("../src/screens/project/trust.tsx");
const settings = await import("../src/screens/project/settings.tsx");
const api = await import("../src/api.ts");

const engine = (file) => readFileSync(new URL(`../../../crates/crazyagent/src/${file}`, import.meta.url), "utf8");

const view = {
  trust: "untrusted",
  origin: "cloned",
  levels: [
    { trust: "reviewed", label: "Reviewed", description: "You made this project or know its code." },
    { trust: "untrusted", label: "Untrusted", description: "Code you have not reviewed." },
  ],
  rules: [{ id: "agent_access", text: "Agents run Sandboxed only.", reason: "Git rights." }],
  gaps: [{ id: "G1", text: "No CPU caps", effect: "Hostile code can slow the machine" }],
  version: 2,
};

test("raising repository trust asks first and lowering saves at once", () => {
  assert.equal(trust.raises("untrusted", "reviewed"), true);
  assert.equal(trust.raises("reviewed", "untrusted"), false);
  assert.equal(trust.raises("untrusted", "untrusted"), false);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /onClick=\{\(\) => view && raises\(view\.trust, trust\) \? setConfirming\(true\) : void save\(false\)\}/);
  assert.match(page, /onConfirm=\{\(\) => void save\(true\)\}/);
  assert.equal(settings.settingsSection("trust"), "trust");
});

test("the trust page names why the project has its level and labels each level as the engine does", () => {
  assert.equal(trust.originLine(view), "Set when CrazyAgent cloned this repository from a URL.");
  assert.equal(trust.originLine({ ...view, origin: "owner" }), "You chose this level.");
  assert.equal(trust.originLine({ ...view, origin: "default" }), "The default for a project you made or added.");
  assert.equal(trust.trustLabel(view, "reviewed"), "Reviewed");
  const source = engine("trust.rs");
  for (const level of view.levels) assert.ok(source.includes(`=> "${level.trust}"`) && source.includes(`=> "${level.label}"`), level.trust);
});

test("Desktop and the TUI say the same raise warning and lowering line", () => {
  const tui = engine("tui/screens/project_trust.rs");
  assert.ok(tui.includes(`"${trust.RAISE_WARNING}"`));
  assert.ok(tui.includes(`"${trust.LOWERING_LINE}"`));
  for (const origin of ["default", "cloned", "owner"]) assert.ok(tui.includes(`"${trust.originLine({ ...view, origin })}"`), origin);
});

test("every engine refusal for repository trust has its own sentence", () => {
  const source = engine("trust.rs");
  for (const code of source.matchAll(/pub const [A-Z_]+_CODE: &str = "([a-z_]+)";/g)) assert.ok(api.KNOWN_ERROR_CODES.includes(code[1]), code[1]);
  for (const code of engine("trust.rs").matchAll(/engine_error\("([a-z_]+)"\)/g)) assert.ok(api.KNOWN_ERROR_CODES.includes(code[1]), code[1]);
  assert.ok(api.KNOWN_ERROR_CODES.includes("agent_access_untrusted"));
});

test("repository names and commit messages with control and bidi characters render inert", async () => {
  const { displayText } = await import("../src/screens/conversation.tsx");
  const hostile = ["evil‮txt.exe", "fix: tidy⁦ rm -rf ~⁩", "name\u0007bell", "a​b", "line break"];
  for (const text of hostile) {
    const shown = displayText(text);
    assert.doesNotMatch(shown, /[\u0000-\u0008\u000b-\u001f\u007f​-‏ -‮⁦-⁩]/u, JSON.stringify(text));
    assert.notEqual(shown, text);
  }
  assert.equal(displayText("plain-repo"), "plain-repo");
});
