import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-process.ts");
const view = await import("../src/screens/process.tsx");
const { parseRoute, formatRoute } = await import("../src/ui/router.ts");

const SKILLS = [
  { id: "skill-000000000000000000000001", name: "tdd", description: "Write the failing test first." },
  { id: "skill-000000000000000000000002", name: "tdd-strict", description: "Red, green, refactor, no exceptions." },
  { id: "skill-000000000000000000000003", name: "code-review", description: "Review a diff for bugs." },
  { id: "skill-000000000000000000000004", name: "bad name", description: "Never a tag." },
];

const never = (what) => () => { throw new Error(`${what} ran during render`); };

function page(extra = {}) {
  return renderToStaticMarkup(createElement(view.ProcessView, {
    scope: { kind: "app" }, saved: { kind: "ok", value: "Use tdd.\n" }, draft: "Use tdd.\n", skills: SKILLS, failure: "", busy: null,
    preview: null, onDraft: never("onDraft"), onSave: never("onSave"), onImport: never("onImport"), onDefault: never("onDefault"),
    onUsePreview: never("onUsePreview"), onClosePreview: never("onClosePreview"), onRetry: never("onRetry"), ...extra,
  }));
}

function saveButton(markup) {
  const button = markup.match(/<button[^>]*type="submit"[^>]*>/)?.[0];
  assert.ok(button, "the page has a Save button");
  return button;
}

test("a catalog skill name reads as a tag only as a whole name, longest first, and the pieces join back", () => {
  const text = "Run tdd-strict, then tdd. Not mytdd or tdd_x. Ask code-review.";
  const segments = client.skillSegments(text, SKILLS);
  assert.equal(segments.map((segment) => segment.kind === "text" ? segment.text : segment.name).join(""), text);
  assert.deepEqual(segments.filter((segment) => segment.kind === "skill"), [
    { kind: "skill", id: "skill-000000000000000000000002", name: "tdd-strict", description: "Red, green, refactor, no exceptions." },
    { kind: "skill", id: "skill-000000000000000000000001", name: "tdd", description: "Write the failing test first." },
    { kind: "skill", id: "skill-000000000000000000000003", name: "code-review", description: "Review a diff for bugs." },
  ]);
  assert.deepEqual(client.skillSegments("bad name here", SKILLS), [{ kind: "text", text: "bad name here" }]);
  assert.deepEqual(client.skillSegments("Use tdd: then tdd.", SKILLS).filter((segment) => segment.kind === "skill").map((segment) => segment.name), ["tdd", "tdd"]);
  assert.deepEqual(client.skillSegments("tdd.x tdd:y tdd--z", SKILLS).filter((segment) => segment.kind === "skill"), []);
  assert.deepEqual(client.skillSegments("tdd", []), [{ kind: "text", text: "tdd" }]);
  assert.deepEqual(client.skillSegments("", SKILLS), []);
});

test("a tag shows its one-line description on hover and opens that skill's detail by id", () => {
  assert.equal(client.skillHref("skill-000000000000000000000003"), "#/settings/plugins?skill=skill-000000000000000000000003");
  const markup = renderToStaticMarkup(createElement(view.TaggedText, { text: "Use code-review now.", skills: SKILLS }));
  assert.match(markup, /<a class="skill-tag" href="#\/settings\/plugins\?skill=skill-000000000000000000000003" title="Review a diff for bugs\." aria-label="Skill code-review: Review a diff for bugs\.">code-review<\/a>/);
  assert.match(markup, /<span>Use <\/span>/);
  assert.match(markup, /<span> now\.<\/span>/);
});

test("Settings and a project each have a Process tab, and a skill link keeps its skill only on Plugins", () => {
  for (const [hash, route] of [
    ["#/settings/process", { name: "settings", tab: "process" }],
    ["#/projects/harbor-notes/process", { name: "project", id: "harbor-notes", tab: "process" }],
    ["#/settings/plugins?skill=skill-0123456789abcdef01234567", { name: "settings", tab: "plugins", skill: "skill-0123456789abcdef01234567" }],
  ]) {
    assert.deepEqual(parseRoute(hash), route, hash);
    assert.equal(formatRoute(route), hash);
  }
  assert.deepEqual(parseRoute("#/settings/process?skill=tdd"), { name: "settings", tab: "process" });
  for (const skill of ["", "bad%20name", "Upper", "a".repeat(65)]) {
    assert.deepEqual(parseRoute(`#/settings/plugins?skill=${skill}`), { name: "settings", tab: "plugins" }, skill);
  }
});

test("the app Process page shows the saved text, the two seeds and a Save that waits for a change", () => {
  const markup = page();
  assert.match(markup, /Instructions every master and sub-agent gets, in every project\./);
  assert.match(markup, /Import from file/);
  assert.match(markup, /Use the CrazyAgent default process/);
  assert.match(markup, /<textarea[^>]*id="process-app"[^>]*>Use tdd\.\n<\/textarea>/);
  assert.match(markup, /9 bytes of 64\.0 KB/);
  assert.doesNotMatch(markup, /Not saved yet/);
  assert.match(saveButton(markup), /disabled=""/);
  assert.match(markup, /As agents read it/);
  assert.match(markup, /class="skill-tag"[^>]*>tdd<\/a>/);
  assert.doesNotMatch(markup, /<dialog/);

  const edited = page({ draft: "Use tdd and code-review.\n" });
  assert.match(edited, /Not saved yet\./);
  assert.doesNotMatch(saveButton(edited), /disabled/);
  assert.match(edited, /Discard edits/);
});

test("a project Process page names its file and offers no seeds", () => {
  const markup = page({ scope: { kind: "project", id: "harbor-notes", name: "Harbor Notes" } });
  assert.match(markup, /Instructions every agent in Harbor Notes gets, after the app-wide ones\. Kept in the project&#x27;s \.crazyagent\/instructions\.md/);
  assert.match(markup, /id="process-project"/);
  assert.doesNotMatch(markup, /Import from file/);
  assert.doesNotMatch(markup, /Use the CrazyAgent default process/);
});

test("a draft over 64 KB or with a NUL says why and cannot be saved", () => {
  const over = "a".repeat(client.PROCESS_TEXT_BYTES_MAX + 1);
  const markup = page({ draft: over });
  assert.match(markup, /This text is over 64 KB\. Shorten it, then save again\./);
  assert.match(saveButton(markup), /disabled=""/);
  assert.equal(client.processTextError("a".repeat(client.PROCESS_TEXT_BYTES_MAX)), undefined);
  assert.equal(client.processTextError("é".repeat(client.PROCESS_TEXT_BYTES_MAX / 2 + 1)), client.processTextError(over));
  assert.match(client.processTextError("a\0b"), /characters CrazyAgent can't save/);
  assert.match(saveButton(page({ draft: "a\0b" })), /disabled=""/);
});

test("a seed opens a preview first and only Put in the editor or Save would change anything", () => {
  const imported = page({ preview: { source: "import", text: "Owner rule: use tdd.\n" } });
  assert.match(imported, /<dialog[^>]*>/);
  assert.match(imported, /Instructions from your file/);
  assert.match(imported, /Read once from the file you picked; afterwards the instructions live here\. It replaces the editor&#x27;s text; nothing is saved until you click Save\./);
  assert.match(imported, /Owner rule: use /);
  assert.match(imported, /Put in the editor/);
  const fallback = page({ preview: { source: "default", text: "# CrazyAgent default process\n" } });
  assert.match(fallback, /CrazyAgent default process/);
  assert.doesNotMatch(fallback, /Read once from the file you picked/);
  assert.match(page({ busy: "import" }), /Reading\.\.\./);
});

test("only the Import from file click opens the chooser, and a closed chooser changes nothing", () => {
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  const reads = [...source.matchAll(/importInstructionsFile\(\)/g)];
  assert.equal(reads.length, 1);
  const seed = source.slice(source.indexOf("const seed = async"), source.indexOf("const save = async"));
  assert.match(seed, /source === "import" \? importInstructionsFile\(\) : defaultProcess\(\)/);
  assert.match(seed, /else if \(result\.value !== null\) setPreview\(\{ source, text: result\.value \}\)/, "a closed chooser opens no preview");
  const api = readFileSync(new URL("../src/api-process.ts", import.meta.url), "utf8");
  assert.match(api, /call<string \| null>\("desktop_import_instructions_file"\)/);
  assert.doesNotMatch(source + api, /importGlobalInstructions|desktop_import_global_instructions/);
  assert.match(source, /onImport=\{\(\) => void seed\("import"\)\}/);
  assert.match(source, /onClick=\{onImport\}/);
});

test("the tags read the engine's one-line description when the catalog carries it", () => {
  const read = { kind: "ok", value: [
    { id: "skill-a", name: "tdd", description: "Long description.\nSecond line.", line: "Write the failing test first." },
    { id: "skill-b", name: "lint", description: "Run the linter." },
  ] };
  assert.deepEqual(view.catalogOf(read), [
    { id: "skill-a", name: "tdd", description: "Write the failing test first." },
    { id: "skill-b", name: "lint", description: "Run the linter." },
  ]);
  assert.deepEqual(view.catalogOf({ kind: "loading" }), []);
  const duplicate = [...SKILLS, { id: "skill-000000000000000000000009", name: "tdd", description: "A project copy." }];
  assert.equal(client.skillSegments("tdd", duplicate)[0].id, "skill-000000000000000000000001", "the first listed skill of a name wins");
});

test("every engine refusal a Process action can show has a plain sentence", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/process.rs", import.meta.url), "utf8");
  const listed = engine.slice(engine.indexOf("PROCESS_CODES: &[&str] = &["), engine.indexOf("];"));
  const codes = [...listed.matchAll(/"((?:process|agent_instructions|instruction_settings)_[a-z_]+)"/g)].map((match) => match[1]);
  for (const code of client.PROCESS_REFUSALS) assert.ok(codes.includes(code), `${code} is not an engine code`);
  for (const code of ["process_text_too_large", "process_text_invalid", "process_text_unsafe", "process_project_folder_missing", "process_import_missing"]) {
    assert.ok(client.PROCESS_REFUSALS.includes(code), code);
    assert.doesNotMatch(client.processMessage({ kind: "error", code, message: "" }), /process_/);
  }
  assert.equal(client.processMessage({ kind: "not_ready" }), "Instructions aren't ready in this build yet.");
});

test("a save a Claude master may refuse names what it takes and what agents now get, and the page shows it", () => {
  const fits = "Instructions saved. Agents started from now on get them.";
  assert.equal(client.savedMessage({ bytes: 24, claude_bytes_max: 28159 }), fits);
  assert.equal(client.savedMessage({ bytes: 28159, claude_bytes_max: 28159 }), fits, "exactly the limit fits");
  assert.equal(client.savedMessage({ bytes: 40000, claude_bytes_max: 28159 }),
    "Instructions saved, but a project's agents now get 39.1 KB of app and project instructions and a Claude master takes only 27.5 KB. Shorten them before a Claude master's next turn.");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/process.rs", import.meta.url), "utf8");
  assert.match(engine, /pub struct ProcessSaved \{\s*pub bytes: u32,\s*pub claude_bytes_max: u32,\s*\}/, "the engine sends the fields this page reads");
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  assert.match(source, /showToast\(savedMessage\(result\.value\)\)/, "the page shows the line for the save it made");
});

test("unsaved instruction text survives a save reply, another client's save and a skill link", () => {
  assert.equal(view.keptDraft("", null, "Saved.\n"), "Saved.\n", "the first read fills the editor");
  assert.equal(view.keptDraft("Saved.\n", "Saved.\n", "From the TUI.\n"), "From the TUI.\n", "an untouched editor follows a save made elsewhere");
  assert.equal(view.keptDraft("Mine, not saved.\n", "Saved.\n", "From the TUI.\n"), "Mine, not saved.\n", "unsaved typing is never replaced");
  assert.equal(view.keptDraft("A then B", "Saved.\n", "A"), "A then B", "text typed after Save is not replaced by the saved part");

  const saving = page({ draft: "Use tdd and code-review.\n", busy: "save" });
  assert.match(saving, /<textarea[^>]*readonly=""/i, "the editor takes no typing while its text is being saved");
  assert.doesNotMatch(page({ draft: "Use tdd and code-review.\n" }), /<textarea[^>]*readonly/i);

  const asked = page({ draft: "Use tdd and code-review.\n", leaving: SKILLS[2] });
  assert.match(asked, /<dialog/);
  assert.match(asked, /Open code-review without saving\?/);
  assert.match(asked, /Your instructions changes will be lost\./);
  assert.match(asked, /Keep editing/);
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  assert.match(source, /<TaggedText text=\{draft\} skills=\{skills\} onOpen=\{onOpenSkill\} \/>/, "a tag under the editor asks before leaving");
  assert.match(source, /<TaggedText text=\{preview\.text\} skills=\{skills\} onOpen=\{onOpenSkill\} \/>/, "a tag in the preview asks before leaving");
});

function agentSettings(extra = {}) {
  return renderToStaticMarkup(createElement(view.AgentSettingsView, {
    settings: { kind: "ok", value: client.DEFAULT_INSTRUCTION_SETTINGS }, busy: false, failure: "", onSave: never("onSave"), ...extra,
  }));
}

const escaped = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const html = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("'", "&#x27;");

test("a project's agent settings offer every instructions mode and the transcript switch, the defaults first", () => {
  assert.deepEqual(client.DEFAULT_INSTRUCTION_SETTINGS, { mode: "both", transcripts: true });
  assert.deepEqual(client.INSTRUCTION_MODES.map((choice) => [choice.value, choice.label]),
    [["project_only", "Project only"], ["app_only", "App only"], ["both", "Both (app, then project appended)"]]);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/instruction_files.rs", import.meta.url), "utf8");
  for (const choice of client.INSTRUCTION_MODES) assert.match(engine, new RegExp(`=> "${choice.value}"`), `${choice.value} is an engine option`);
  const markup = agentSettings();
  assert.match(markup, /Instructions agents get/);
  for (const choice of client.INSTRUCTION_MODES) assert.match(markup, new RegExp(`role="radio"[^>]*>${escaped(choice.label)}</button>`));
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>Both \(app, then project appended\)</);
  assert.ok(markup.includes(html(client.INSTRUCTION_MODES[2].note)));
  assert.match(markup, /role="switch"[^>]*aria-checked="true"/);
  assert.match(markup, /Keep transcripts for headless Claude runs/);
  assert.match(markup, /keep their transcript in the agent&#x27;s own home/);
  assert.doesNotMatch(markup, /Project files|Project and user files/);
  for (const choice of client.INSTRUCTION_MODES) {
    const shown = agentSettings({ settings: { kind: "ok", value: { mode: choice.value, transcripts: false } } });
    assert.match(shown, new RegExp(`role="radio" aria-checked="true"[^>]*>${escaped(choice.label)}</`));
    assert.ok(shown.includes(html(choice.note)), choice.value);
    assert.match(shown, /role="switch"[^>]*aria-checked="false"/);
    assert.match(shown, /keep no transcript/);
  }
});

test("unreadable agent settings say why and show the defaults so a change replaces them", () => {
  const markup = agentSettings({ settings: { kind: "error", code: "instruction_settings_invalid", message: "" } });
  assert.match(markup, /value CrazyAgent doesn&#x27;t know/);
  assert.doesNotMatch(markup, /instruction_settings_/);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>Both \(app, then project appended\)</);
  assert.match(agentSettings({ failure: "This project's agent settings couldn't be read. Try again." }), /couldn&#x27;t be read/);
  assert.match(agentSettings({ busy: true }), /Saving\.\.\./);
  assert.match(agentSettings({ settings: { kind: "not_ready" } }), /ready/);
  for (const code of ["instruction_settings_invalid", "instruction_settings_unavailable"]) assert.ok(client.PROCESS_REFUSALS.includes(code), code);
});

test("a project's Process page reads and saves its agent settings through the engine commands", () => {
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  assert.match(source, /<AgentSettings projectId=\{scope\.id\} \/>/);
  assert.match(source, /if \(scope\.kind === "app"\) return editor;/, "the app page has no agent settings");
  const api = readFileSync(new URL("../src/api-process.ts", import.meta.url), "utf8");
  assert.match(api, /call<InstructionSettings>\("desktop_instruction_settings", \{ projectId \}\)/);
  assert.match(api, /call<InstructionSettings>\("desktop_save_instruction_settings", \{ projectId, settings \}\)/);
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const command of ["desktop_instruction_settings", "desktop_save_instruction_settings"]) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`));
    assert.match(lib, new RegExp(`\\n\\s+${command},\\n`), `${command} is registered`);
  }
});

test("picking an instructions mode or the transcript switch saves the whole setting as mode and transcripts, never a files field", async () => {
  const { bundle, withPage } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/agent-settings.mjs", import.meta.url).pathname);
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(client.DEFAULT_INSTRUCTION_SETTINGS)})`);
    const clicks = [];
    for (const label of ["Project only", "App only", "agent-transcripts"]) clicks.push(await evaluate(`Fixture.click(${JSON.stringify(label)})`));
    return { clicks, saves: await evaluate("Fixture.saves()") };
  });
  assert.deepEqual(seen.clicks, [true, true, true]);
  assert.deepEqual(seen.saves, [
    { mode: "project_only", transcripts: true },
    { mode: "app_only", transcripts: true },
    { mode: "both", transcripts: false },
  ]);
});

const importFile = (path, extra = {}) => ({ path, bytes: 15, tracked: false, refused: null, uncommitted: false, ...extra });

test("the import prompt lists each found file with its size, a tracked note or why it stays, and confirms only a movable set", () => {
  const prompt = (files, extra = {}) => renderToStaticMarkup(createElement(view.InstructionImportView, {
    name: "Harbor Notes", files, busy: false, failure: "", onConfirm: never("onConfirm"), onLater: never("onLater"), ...extra,
  }));
  const markup = prompt([
    importFile("CLAUDE.md", { tracked: true }),
    importFile("AGENTS.md", { bytes: 2048 }),
    importFile("CLAUDE.local.md", { refused: "process_import_outside" }),
  ]);
  assert.match(markup, /Instruction files found in Harbor Notes/);
  assert.match(markup, /<strong>CLAUDE\.md<\/strong> <span class="secondary">15 bytes, tracked by git<\/span>/);
  assert.match(markup, /<strong>AGENTS\.md<\/strong> <span class="secondary">2\.0 KB<\/span>/);
  assert.ok(markup.includes(html(`Stays in place: ${client.importRefusal("process_import_outside")}`)), markup);
  assert.match(markup, /Git tracks some of these files, so after the move your checkout shows them as deleted\./);
  assert.match(markup, /Not now/);
  assert.doesNotMatch(markup.match(/<button[^>]*>(?:(?!<\/button>).)*Copy into instructions\.md and move aside/s)[0], /disabled/);
  const refused = prompt([importFile("CLAUDE.md", { refused: "process_import_outside" })]);
  assert.match(refused.match(/<button[^>]*>(?:(?!<\/button>).)*Copy into instructions\.md and move aside/s)[0], /disabled=""/, "nothing movable, nothing to confirm");
  assert.doesNotMatch(refused, /Git tracks some/);
  assert.match(prompt([importFile("AGENTS.md")], { failure: "The instruction files couldn't be moved aside." }), /class="notice danger" role="alert">.*The instruction files couldn&#x27;t be moved aside\./s);
});

test("the move notice names the files, says this or these deletions, offers Leave it only when given and shows a refusal as an alert", () => {
  const notice = (files, extra = {}) => renderToStaticMarkup(createElement(view.ImportMoveView, {
    files, busy: false, failure: "", onCommit: never("onCommit"), ...extra,
  }));
  const one = notice(["CLAUDE.md"], { onLeave: never("onLeave") });
  assert.match(one, /Git still shows CLAUDE\.md as deleted, and merges into this branch are refused until the move is committed\./);
  assert.match(one, /holding only this deletion; it is never pushed\./);
  assert.match(one, />Leave it</);
  assert.match(one, />Commit the move</);
  assert.doesNotMatch(one, /role="alert"/);
  const two = notice(["CLAUDE.md", ".claude/CLAUDE.md"]);
  assert.match(two, /Git still shows CLAUDE\.md, \.claude\/CLAUDE\.md as deleted/);
  assert.match(two, /holding only these deletions;/);
  assert.doesNotMatch(two, /Leave it/, "the banner offers no Leave it");
  const refused = notice(["CLAUDE.md"], { failure: client.processMessage({ kind: "error", code: "process_commit_staged", message: "" }) });
  assert.match(refused, /class="import-move-failure" role="alert">Other files are staged/);
  for (const code of ["process_commit_staged", "process_commit_none", "process_commit_busy", "process_commit_detached", "process_commit_moved", "process_commit_identity", "process_commit_failed"]) {
    assert.ok(client.PROCESS_REFUSALS.includes(code), code);
    assert.doesNotMatch(client.processMessage({ kind: "error", code, message: "" }), /process_/, code);
  }
});
