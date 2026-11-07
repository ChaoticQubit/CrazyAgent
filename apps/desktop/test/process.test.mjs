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

const PROJECT = { kind: "project", id: "harbor-notes", name: "Harbor Notes" };

function projectPage(extra = {}) {
  return page({
    scope: PROJECT, view: "read", all: false, given: { kind: "ok", value: "App rules.\n\nUse tdd.\n" }, subtitle: "App default plus this project's file",
    editNote: "Running agents get the change at their next task.", onView: never("onView"), onAll: never("onAll"), onUseDefault: never("onUseDefault"), ...extra,
  });
}

test("a project's Process page shows the instructions as agents read them, folded, with Edit beside and no seeds", () => {
  const markup = projectPage();
  const words = markup.replace(/<[^>]*>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");
  assert.match(words, /Instructions App default plus this project's file · 3 lines/);
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>As agents read it<\/button>/);
  assert.match(markup, /role="radio" aria-checked="false"[^>]*>Edit<\/button>/);
  assert.match(markup, /App rules\./);
  assert.match(markup, /class="skill-tag"[^>]*>tdd<\/a>/);
  assert.doesNotMatch(markup, /<textarea/);
  assert.doesNotMatch(markup, /Show all|Import from file|Use the CrazyAgent default process/);
  const long = Array.from({ length: 20 }, (_, index) => `Line ${index + 1}.`).join("\n");
  assert.match(projectPage({ given: { kind: "ok", value: long } }), /class="pr-instr"[\s\S]*>Show all 20 lines<\/button>/);
  assert.match(projectPage({ given: { kind: "ok", value: long }, all: true }), /class="pr-instr all"[\s\S]*>Show less<\/button>/);
});

const GIVEN = "# Instructions from CrazyAgent\n\n## App instructions\n\nApp rules.\n\n## Project instructions\n\nUse tdd.\n";
const plain = (markup) => markup.replace(/<[^>]*>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

test("As agents read it shows the text the engine gives agents, title and headings too, and says when the engine cannot read it", () => {
  const markup = projectPage({ given: { kind: "ok", value: GIVEN } });
  assert.match(plain(markup), /Instructions App default plus this project's file · 9 lines/);
  assert.match(markup, /# Instructions from CrazyAgent/);
  assert.match(markup, /## Project instructions/);
  const failed = projectPage({ given: { kind: "error", code: "process_text_unsafe", message: "process_text_unsafe" } });
  assert.match(failed, /role="alert"/);
  assert.match(plain(failed), /What agents get could not be read\. The instructions file is a link or not a plain file, so CrazyAgent won't read it\. Replace it with a plain file\./);
  assert.doesNotMatch(failed, /class="pr-instr/);
  assert.doesNotMatch(plain(failed), /· \d+ lines/);
  assert.match(plain(projectPage({ given: { kind: "ok", value: "" } })), /Agents get no instructions from you now\./);
  assert.match(projectPage({ given: { kind: "loading" } }), /Reading what agents get/);
});

test("under Project only, Use app default is not offered and an empty file cannot be saved, so agents are never left with no instructions", () => {
  const only = projectPage({ view: "edit", mode: "project_only", subtitle: "This project's file only" });
  assert.doesNotMatch(only, />Use app default<\/button>/);
  assert.match(plain(only), /Project only gives agents this file alone\. To use the app default, pick Both or App only under Delivery\./);
  const empty = projectPage({ view: "edit", mode: "project_only", draft: "" });
  assert.match(saveButton(empty), /disabled=""/);
  assert.match(plain(empty), /Under Project only an empty file leaves agents with no instructions\./);
  assert.match(projectPage({ view: "edit", mode: "both" }), />Use app default<\/button>/);
  assert.doesNotMatch(projectPage({ view: "edit", mode: null }), />Use app default<\/button>/, "until Delivery is read the mode is unknown, so the button is not offered");
  assert.doesNotMatch(saveButton(projectPage({ view: "edit", mode: "both", draft: "" })), /disabled/);
  assert.doesNotMatch(saveButton(projectPage({ view: "edit", mode: "app_only", draft: "" })), /disabled/);
});

test("Edit shows the project's own text with Save, Use app default and when running agents get the change", () => {
  const markup = projectPage({ view: "edit" });
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>Edit<\/button>/);
  assert.match(markup, /<textarea[^>]*id="process-project"[^>]*>Use tdd\.\n<\/textarea>/);
  assert.match(saveButton(markup), /disabled=""/);
  assert.match(markup, />Use app default<\/button>/);
  assert.match(markup, /Running agents get the change at their next task\./);
  assert.doesNotMatch(markup, /Import from file|Use the CrazyAgent default process/);
  assert.match(projectPage({ view: "edit", draft: "" }), /disabled=""[^>]*>Use app default<\/button>/, "an empty project text is the app default already");
  const stale = projectPage({ conflict: { opened: 1, version: 2, current: "Theirs.\n", writer: "owner" } });
  assert.match(stale, /<textarea[^>]*id="process-project"/, "a stale save shows the editor whatever view was picked");
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
  assert.equal(client.savedMessage(null), fits, "a save that reports no size shows the plain line");
  assert.equal(client.savedMessage({ bytes: 28159, claude_bytes_max: 28159 }), fits, "exactly the limit fits");
  assert.equal(client.savedMessage({ bytes: 40000, claude_bytes_max: 28159 }),
    "Instructions saved, but a project's agents now get 39.1 KB of app and project instructions and a Claude master takes only 27.5 KB. Shorten them before a Claude master's next turn.");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/process.rs", import.meta.url), "utf8");
  assert.match(engine, /pub struct ProcessSaved \{\s*pub bytes: u32,\s*pub claude_bytes_max: u32,\s*\}/, "the engine sends the fields this page reads");
  assert.match(engine, /Saved \{\s*version: u64,\s*size: Option<ProcessSaved>,\s*\}/, "a versioned save carries the size beside its version");
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  assert.match(source, /showToast\(savedMessage\(result\.value\.size\)\)/, "the page shows the line for the save it made");
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
    settings: { kind: "ok", value: { settings: client.DEFAULT_INSTRUCTION_SETTINGS, version: 0 } }, busy: false, failure: "", onSave: never("onSave"), ...extra,
  }));
}

const escaped = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const html = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("'", "&#x27;");

test("a project's agent settings offer every instructions mode and the transcript switch, the defaults first", () => {
  assert.deepEqual(client.DEFAULT_INSTRUCTION_SETTINGS, { mode: "both", transcripts: true, edits: "next_message" });
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
    const shown = agentSettings({ settings: { kind: "ok", value: { settings: { mode: choice.value, transcripts: false }, version: 3 } } });
    assert.match(shown, new RegExp(`role="radio" aria-checked="true"[^>]*>${escaped(choice.label)}</`));
    assert.ok(shown.includes(html(choice.note)), choice.value);
    assert.match(shown, /role="switch"[^>]*aria-checked="false"/);
    assert.match(shown, /keep no transcript/);
  }
});

test("every way an instruction edit reaches running agents is offered with what it does, Next task first", () => {
  assert.deepEqual(client.EDIT_DELIVERIES.map((choice) => [choice.value, choice.label]),
    [["next_message", "Next task"], ["restart", "Restart when idle"], ["badge", "Badge only"]]);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/instruction_files.rs", import.meta.url), "utf8");
  for (const choice of client.EDIT_DELIVERIES) assert.match(engine, new RegExp(`=> "${choice.value}"`), `${choice.value} is an engine option`);
  const markup = agentSettings();
  assert.match(markup, /When you edit instructions/);
  for (const choice of client.EDIT_DELIVERIES) assert.match(markup, new RegExp(`role="radio"[^>]*>${escaped(choice.label)}</button>`));
  assert.match(markup, /role="radio" aria-checked="true"[^>]*>Next task</);
  assert.match(markup, /next task starts with the sections you changed/);
  for (const choice of client.EDIT_DELIVERIES) {
    const shown = agentSettings({ settings: { kind: "ok", value: { settings: { mode: "both", transcripts: true, edits: choice.value }, version: 0 } } });
    assert.match(shown, new RegExp(`role="radio" aria-checked="true"[^>]*>${escaped(choice.label)}</`));
    assert.ok(shown.includes(html(choice.note)), choice.value);
  }
  assert.match(agentSettings({ settings: { kind: "ok", value: { settings: { mode: "both", transcripts: true }, version: 0 } } }),
    /role="radio" aria-checked="true"[^>]*>Next task</, "an engine reply without the choice shows the default");
  assert.match(client.EDIT_DELIVERIES[1].note, /restarts when it is next idle/);
  assert.match(client.EDIT_DELIVERIES[2].note, /shows Instructions changed/);
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
  assert.match(source, /<AgentSettings projectId=\{scope\.id\} onSaved=\{reloadSettings\} \/>/);
  assert.match(source, /if \(scope\.kind === "app"\) return editor;/, "the app page has no agent settings");
  const api = readFileSync(new URL("../src/api-process.ts", import.meta.url), "utf8");
  assert.match(api, /call<InstructionView>\("desktop_instruction_settings", \{ projectId \}\)/);
  assert.match(api, /call<InstructionView>\("desktop_save_instruction_settings", \{ projectId, settings, version \}\)/);
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const command of ["desktop_instruction_settings", "desktop_save_instruction_settings"]) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`));
    assert.match(lib, new RegExp(`\\n\\s+${command},\\n`), `${command} is registered`);
  }
});

test("an agent settings save names the version the page read, and a save refused as stale shows the saved settings again", () => {
  const read = { settings: { mode: "app_only", transcripts: false, edits: "next_message" }, version: 7 };
  assert.equal(client.settingsSaveVersion({ kind: "ok", value: read }), 7, "a save names the version read");
  assert.equal(client.settingsSaveVersion({ kind: "error", code: "instruction_settings_invalid", message: "" }), null,
    "unreadable settings are replaced with no version");
  assert.equal(client.AGENT_SETTINGS_STALE,
    "These agent settings changed since you opened them. The page now shows the saved ones; make your change again.");
  assert.match(agentSettings({ settings: { kind: "ok", value: read }, failure: client.AGENT_SETTINGS_STALE }), /changed since you opened them/);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/process.rs", import.meta.url), "utf8");
  const listed = engine.slice(engine.indexOf("PROCESS_CODES: &[&str] = &["), engine.indexOf("];"));
  assert.match(listed, /"stale_revision"/, "the engine can refuse a settings save as stale");
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  assert.match(source, /saveInstructionSettings\(projectId, next, settingsSaveVersion\(settings\)\)/);
  assert.match(source, /result\.code === "stale_revision"\) \{\s*reload\(\);\s*setFailure\(AGENT_SETTINGS_STALE\);/);
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(lib, /async fn desktop_save_instruction_settings\([^)]*version: Option<u64>/);
});

test("picking an instructions mode, the transcript switch or an edit choice saves the whole setting, never a files field", async () => {
  const { bundle, withPage } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/agent-settings.mjs", import.meta.url).pathname);
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(client.DEFAULT_INSTRUCTION_SETTINGS)})`);
    const clicks = [];
    for (const label of ["Project only", "App only", "agent-transcripts", "Restart when idle", "Badge only"]) {
      clicks.push(await evaluate(`Fixture.click(${JSON.stringify(label)})`));
    }
    const first = await evaluate("Fixture.saves()");
    await evaluate(`Fixture.mount(${JSON.stringify({ ...client.DEFAULT_INSTRUCTION_SETTINGS, edits: "badge" })})`);
    clicks.push(await evaluate(`Fixture.click("Next task")`));
    return { clicks, saves: [...first, ...await evaluate("Fixture.saves()")] };
  });
  assert.deepEqual(seen.clicks, [true, true, true, true, true, true]);
  assert.deepEqual(seen.saves, [
    { mode: "project_only", transcripts: true, edits: "next_message" },
    { mode: "app_only", transcripts: true, edits: "next_message" },
    { mode: "both", transcripts: false, edits: "next_message" },
    { mode: "both", transcripts: true, edits: "restart" },
    { mode: "both", transcripts: true, edits: "badge" },
    { mode: "both", transcripts: true, edits: "next_message" },
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

test("the commit toast names the branch and files and shows a direction or zero-width mark in the branch as an escape", () => {
  assert.equal(client.committedMessage({ branch: "main", files: ["CLAUDE.md", "AGENTS.md"] }), "Committed the move on main: CLAUDE.md, AGENTS.md. Nothing was pushed.");
  assert.equal(client.committedMessage({ branch: "rel\u202e\u200bx", files: ["CLAUDE.md"] }), "Committed the move on rel\\u202e\\u200bx: CLAUDE.md. Nothing was pushed.");
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
  const named = notice(["CLAUDE.md"], { branch: "trial4-land" });
  assert.match(named, /Git still shows CLAUDE\.md as deleted, and merges into trial4-land are refused until the move is committed\./, "the banner names the checked-out branch");
  assert.doesNotMatch(named, /this branch/);
  assert.match(notice(["CLAUDE.md"], { branch: null }), /merges into this branch are refused/, "a detached HEAD has no branch to name");
  assert.match(notice(["CLAUDE.md"], { branch: "<b>x" }), /merges into &lt;b&gt;x are refused/, "the branch is text, never markup");
  const hidden = notice(["CLAUDE.md"], { branch: "main\u202e\u200bx" });
  assert.match(hidden, /merges into main\\u202e\\u200bx are refused/, "a direction or zero-width mark in the branch shows as an escape");
  assert.ok(!hidden.includes("\u202e") && !hidden.includes("\u200b"), hidden);
  const refused = notice(["CLAUDE.md"], { failure: client.processMessage({ kind: "error", code: "process_commit_staged", message: "" }) });
  assert.match(refused, /class="import-move-failure" role="alert">Other files are staged/);
  for (const code of ["process_commit_staged", "process_commit_none", "process_commit_busy", "process_commit_detached", "process_commit_moved", "process_commit_identity", "process_commit_failed"]) {
    assert.ok(client.PROCESS_REFUSALS.includes(code), code);
    assert.doesNotMatch(client.processMessage({ kind: "error", code, message: "" }), /process_/, code);
  }
});

const ADOPTION = {
  nudges: "ten_minutes",
  counted: ["start_work", "graphify"],
  skills: ["tdd", "zeta"],
  hidden: 2,
  agents: [
    { agent: 3, role: "sub", harness: "claude-code", active: true, counts: [0, 1], skills: [1, 0], other_skills: [], graphify: 1, context_mode: 0,
      flags: ["edits_without_start_work"], reminded: [], hookless: true },
    { agent: 2, role: "sub", harness: "codex", active: false, counts: [0, 2], skills: [1, 0], other_skills: [{ name: "other-one", count: 1 }], graphify: 2, context_mode: 1,
      flags: ["edits_without_start_work"], reminded: ["edits_without_start_work"], hookless: false },
    { agent: 1, role: "master", harness: "claude-code", active: true, counts: [1, 0], skills: [0, 0], other_skills: [], graphify: 0, context_mode: 0,
      flags: ["no_graphify_or_context_mode"], reminded: [], hookless: false },
  ],
};

const NAMES = new Map([[1, "Master"], [2, "Moss"], [3, "Wren"]]);

const followRow = (markup, name) => {
  const row = markup.split("<tr").find((part) => part.includes(`>${name}</button>`));
  assert.ok(row, `${name} has a row`);
  return row;
};

function following(extra = {}) {
  return renderToStaticMarkup(createElement(view.FollowingView, {
    adoption: { kind: "ok", value: ADOPTION }, names: NAMES, finished: false, expanded: null,
    onFinished: never("onFinished"), onExpand: never("onExpand"), onMetrics: never("onMetrics"), onRetry: never("onRetry"), ...extra,
  }));
}

const words = (markup) => markup.replace(/<[^>]*>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

test("Are agents following it shows one row per agent with its graph, context tools, skills and flags, and sends counts to Metrics", () => {
  const markup = following();
  assert.match(words(markup), /Are agents following it One row per agent\. Click a row for details\. Counts are in Metrics\. Open Metrics/);
  assert.match(markup, /<th scope="col">Agent<\/th><th scope="col">Graph before edits<\/th><th scope="col">Context tools<\/th><th scope="col">Skills used<\/th><th scope="col">Flags<\/th>/);
  assert.doesNotMatch(markup, /<th scope="col">(start_work|graphify)<\/th>/, "the counts moved to Metrics");
  assert.doesNotMatch(markup, /Nudges/);
  assert.match(words(followRow(markup, "Wren")), /Wren Claude Code Missed Not yet tdd Edits without start_work/);
  assert.match(words(followRow(markup, "Master")), /Master Claude Code Not yet Missed None 10\+ min without graphify or context-mode/);
  assert.match(markup, /2 older agents are not listed\./);
  assert.match(markup, /aria-expanded="false"/);
});

test("finished agents wait behind Show finished, and a clicked row shows the agent's counts", () => {
  const markup = following();
  assert.doesNotMatch(markup, /Moss/);
  assert.match(markup, />Show finished \(1\)<\/button>/);
  const all = following({ finished: true });
  assert.match(words(followRow(all, "Moss")), /Moss Codex Missed Yes tdd, other-one Edits without start_work reminded/);
  assert.match(all, />Hide finished<\/button>/);
  const open = following({ expanded: 3 });
  assert.match(open, /aria-expanded="true"/);
  const detail = open.slice(open.indexOf('colSpan="5"') >= 0 ? open.indexOf('colSpan="5"') : open.indexOf('colspan="5"'));
  assert.match(detail, /start_work <strong>0<\/strong>/);
  assert.match(detail, /graphify <strong>1<\/strong>/);
  assert.match(detail, /<span>tdd <strong>1<\/strong><\/span>/);
  const none = following({ adoption: { kind: "ok", value: { ...ADOPTION, hidden: 0, agents: [] } } });
  assert.match(none, /No agents yet/);
  assert.doesNotMatch(none, /Show finished|not listed/);
  const failed = following({ adoption: { kind: "error", code: "process_adoption_unavailable", message: "" }, onRetry() {} });
  assert.match(failed, /Workflow adoption couldn&#x27;t be read\. Try again\./);
});

test("the project Process page puts Delivery and How a task runs beside the instructions and who follows them", () => {
  assert.match(agentSettings(), /<h2>Delivery<\/h2>/);
  const steps = words(renderToStaticMarkup(createElement(view.TaskSteps)));
  assert.match(steps, /How a task runs 1 Master writes the task and picks an agent\. 2 Agent reads instructions, asks the graph, works on its branch\. 3 Reviewer checks the change set\. 4 Master merges into main \(Full access\) or it waits for your accept\./);
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  assert.match(source, /<div className="pr">[\s\S]*<ProjectFollowing projectId=\{scope\.id\}[\s\S]*<aside className="pr-side">\s*<AgentSettings projectId=\{scope\.id\} onSaved=\{reloadSettings\} \/>\s*<TaskSteps \/>/);
  assert.doesNotMatch(source, /<ProjectAdoption|function AdoptionView/);
});

test("an agent's skill counts list graphify, ctx_* and every enabled skill, zeros muted", () => {
  const counts = renderToStaticMarkup(createElement(view.SkillCounts, { skills: ADOPTION.skills, agent: ADOPTION.agents[1] }));
  assert.deepEqual([...counts.matchAll(/<span( class="secondary")?>([^<]+) <strong>(\d+)<\/strong><\/span>/g)].map((found) => [found[2], Number(found[3]), Boolean(found[1])]), [
    ["graphify", 2, false], ["ctx_*", 1, false], ["tdd", 1, false], ["zeta", 0, true], ["other-one (not enabled)", 1, false],
  ]);
  const none = renderToStaticMarkup(createElement(view.SkillCounts, { skills: [], agent: ADOPTION.agents[2] }));
  assert.match(none, /<span class="secondary">No skills enabled<\/span>/);
});

test("the skills dialog shows an agent's counts, a hookless agent's too, or says why an unlisted agent has none", () => {
  const body = (agent, adoption = { kind: "ok", value: ADOPTION }) =>
    renderToStaticMarkup(createElement(view.AgentSkills, { adoption, agent, onRetry() {} }));
  assert.match(body(2), /tdd <strong>1<\/strong>/);
  assert.match(body(3), /<span>tdd <strong>1<\/strong><\/span>/);
  assert.doesNotMatch(body(3), /unavailable/);
  assert.match(body(9), /This agent is older than the agents the Process page lists, so its counts are not shown\./);
  assert.match(body(2, { kind: "error", code: "process_adoption_unavailable", message: "" }), /Workflow adoption couldn&#x27;t be read\. Try again\./);
});

test("a save the engine refuses because agents would get no instructions says so in one sentence under Delivery", () => {
  const refused = client.processMessage({ kind: "error", code: "process_instructions_would_be_empty", message: "" });
  assert.equal(refused, "That would leave agents with no instructions, so nothing was saved; write the instructions they should get first.");
  assert.ok(client.PROCESS_REFUSALS.includes("process_instructions_would_be_empty"));
  assert.match(agentSettings({ failure: refused }), /class="notice danger" role="alert">.*That would leave agents with no instructions, so nothing was saved; write the instructions they should get first\./s);
});
