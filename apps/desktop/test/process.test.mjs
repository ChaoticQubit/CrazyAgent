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
  assert.match(markup, /Import my global instructions/);
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
  assert.doesNotMatch(markup, /Import my global instructions/);
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
  assert.match(imported, /Your global instructions/);
  assert.match(imported, /Read from ~\/\.claude\/CLAUDE\.md\. It replaces the editor&#x27;s text; nothing is saved until you click Save\./);
  assert.match(imported, /Owner rule: use /);
  assert.match(imported, /Put in the editor/);
  const fallback = page({ preview: { source: "default", text: "# CrazyAgent default process\n" } });
  assert.match(fallback, /CrazyAgent default process/);
  assert.doesNotMatch(fallback, /Read from ~\/\.claude/);
  assert.match(page({ busy: "import" }), /Reading\.\.\./);
});

test("only the Import click reads the owner's global instructions", () => {
  const source = readFileSync(new URL("../src/screens/process.tsx", import.meta.url), "utf8");
  const reads = [...source.matchAll(/importGlobalInstructions\(\)/g)];
  assert.equal(reads.length, 1);
  const seed = source.slice(source.indexOf("const seed = async"), source.indexOf("const save = async"));
  assert.match(seed, /source === "import" \? importGlobalInstructions\(\) : defaultProcess\(\)/);
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
  const codes = [...listed.matchAll(/"(process_[a-z_]+)"/g)].map((match) => match[1]);
  for (const code of client.PROCESS_REFUSALS) assert.ok(codes.includes(code), `${code} is not an engine code`);
  for (const code of ["process_text_too_large", "process_text_invalid", "process_text_unsafe", "process_project_folder_missing", "process_import_missing"]) {
    assert.ok(client.PROCESS_REFUSALS.includes(code), code);
    assert.doesNotMatch(client.processMessage({ kind: "error", code, message: "" }), /process_/);
  }
  assert.equal(client.processMessage({ kind: "not_ready" }), "Instructions aren't ready in this build yet.");
});
