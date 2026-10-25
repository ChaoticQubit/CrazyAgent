import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      for (const extension of [".ts", ".tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) {
      return nextLoad(url, context);
    }
    const result = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(result.errors, [], `production module did not transform: ${url}`);
    return { format: "module", source: result.code, shortCircuit: true };
  },
});

const application = await import("../src/screens/legacy.tsx");
const { useOwnerBrowser } = await import("../src/state.ts");

test("Projects navigation renders a bounded declaration form without owner authority", () => {
  function ProjectsView() {
    const state = useOwnerBrowser();
    return createElement(application.OwnerView, { state: { ...state, view: "projects" } });
  }
  const markup = renderToStaticMarkup(createElement(ProjectsView));
  assert.match(markup, /<form\b[^>]*aria-label="Create project"/);
  assert.match(markup, /<label\b[^>]*for="project-name"/);
  assert.match(markup, /<input\b[^>]*id="project-name"[^>]*maxLength="256"/);
  assert.match(markup, /<label\b[^>]*for="project-repositories"/);
  assert.match(markup, /<textarea\b[^>]*id="project-repositories"/);
  assert.match(markup, /<button\b[^>]*disabled=""[^>]*>Create project<\/button>/);
  assert.match(markup, /declar(?:ed|ation)/i);
  assert.match(markup, /does not[^<]*(?:start|run|launch)/i);
  assert.doesNotMatch(markup, /<input\b[^>]*name="(?:project_id|request_id|expected_revision|owner_id)"/);
  assert.ok(Buffer.byteLength(markup) < 32768);
});

const creation = await import("../src/screens/projects.tsx");

test("production project parser preserves exact declarations and UTF-8 boundaries", async () => {
  const name = "é".repeat(128);
  const path = "/" + "é".repeat(2047) + "a";
  assert.deepEqual(creation.parseProjectSpec(name, path), { name, repositories: [path] });
  assert.deepEqual(creation.parseProjectSpec(" Project ", "/one path\r\n/two\n/one path"), {
    name: " Project ", repositories: ["/one path", "/two", "/one path"],
  });
  assert.equal(creation.parseProjectSpec("Bounded", Array(64).fill("/repo").join("\n")).repositories.length, 64);
  assert.throws(() => creation.parseProjectSpec("é".repeat(129), "/repo"), /256/);
  assert.throws(() => creation.parseProjectSpec("Project", path + "é"), /4096/);
  const { displayText } = await import("../src/screens/conversation.tsx");
  const declared = creation.parseProjectSpec("Project\u202e", "/repo\u2066");
  assert.deepEqual(declared, { name: "Project\u202e", repositories: ["/repo\u2066"] });
  const displayed = displayText(JSON.stringify(declared, null, 2));
  assert.match(displayed, /\\u202e/);
  assert.match(displayed, /\\u2066/);
  assert.doesNotMatch(displayed, /[\u202e\u2066]/u);
});

test("displayText shows zero-width, tag, separator and stray selector characters and keeps emoji whole", async () => {
  const { displayText } = await import("../src/screens/conversation.tsx");
  assert.equal(displayText("Use\u200bpnpm"), "Use\\u200bpnpm");
  assert.equal(displayText("Use pnpm\u{e0041}\u{e0042}"), "Use pnpm\\u{e0041}\\u{e0042}");
  assert.equal(displayText("a\u2028b\u2029c\u00add\ufeffe\u2060f\u180eg"), "a\\u2028b\\u2029c\\u00add\\ufeffe\\u2060f\\u180eg");
  assert.equal(displayText("a\ufe0fb\u{e0100}"), "a\\ufe0fb\\u{e0100}");
  assert.equal(displayText("\u{1f468}\u200d\u200d\u{1f469}"), "\u{1f468}\\u200d\\u200d\u{1f469}");
  assert.equal(displayText("a\u200db\u200c"), "a\\u200db\\u200c");
  const emoji = "Family \u{1f468}\u200d\u{1f469}\u200d\u{1f467}, \u26a0\ufe0f warn, \u2764\ufe0f\u200d\u{1f525}\nnext\tline";
  assert.equal(displayText(emoji), emoji);
  assert.equal(displayText("\u26a0\ufe0f\ufe0f"), "\u26a0\\ufe0f\\ufe0f");
});

test("production parser refuses missing, relative, URL, control and oversized declarations", () => {
  for (const name of ["", " \t", "line\nbreak", "nul\0", "control\u0085"]) {
    assert.throws(() => creation.parseProjectSpec(name, "/repo"));
  }
  for (const paths of ["", "repo", "./repo", "~/repo", "https://host/repo", "/repo\n", "/repo\n\n/other", "/repo\t", "/repo\u0085", "/scheme://repo", Array(65).fill("/repo").join("\n"), Array(64).fill("/" + "x".repeat(4095)).join("\n")]) {
    assert.throws(() => creation.parseProjectSpec("Project", paths));
  }
});

test("production creation reducer isolates replacement and cancel generations", () => {
  const original = { name: "Original", repositories: "/original", generation: 4 };
  const edited = creation.reduceProjectDraft(original, { kind: "edit", field: "name", value: "Replacement", generation: 5 });
  assert.deepEqual(original, { name: "Original", repositories: "/original", generation: 4 });
  assert.deepEqual(edited, { name: "Replacement", repositories: "/original", generation: 5 });
  assert.equal(creation.creationMatches(original, { generation: 4, navigation: 7 }, 7), true);
  assert.equal(creation.creationMatches(edited, { generation: 4, navigation: 7 }, 7), false);
  assert.equal(creation.creationMatches(original, { generation: 4, navigation: 7 }, 8), false);
  assert.equal(creation.creationMatches(original, undefined, 7), false);
  const cancelled = creation.reduceProjectDraft(edited, { kind: "discard", generation: 6 });
  assert.deepEqual(cancelled, { name: "", repositories: "", generation: 6 });
  assert.equal(creation.creationMatches(cancelled, { generation: 5, navigation: 7 }, 7), false);
  assert.throws(() => creation.reduceProjectDraft(edited, { kind: "discard", generation: 5 }), /generation/);
  assert.throws(() => creation.reduceProjectDraft(edited, { kind: "edit", field: "name", value: "x".repeat(257), generation: 6 }), /256/);
  assert.throws(() => creation.reduceProjectDraft(edited, { kind: "edit", field: "repositories", value: "x".repeat(65537), generation: 6 }), /65536/);
  assert.deepEqual(edited, { name: "Replacement", repositories: "/original", generation: 5 });
  const returned = creation.reduceProjectDraft(cancelled, { kind: "edit", field: "name", value: "Original", generation: 7 });
  assert.equal(creation.creationMatches(returned, { generation: 4, navigation: 7 }, 7), false);
  for (const generation of [0, -1, 5, 6.5, Number.MAX_SAFE_INTEGER + 1, NaN]) {
    assert.throws(() => creation.reduceProjectDraft(cancelled, { kind: "discard", generation }), /generation/);
  }
  assert.throws(() => creation.reduceProjectDraft(cancelled, { kind: "edit", field: "repositories", value: "/nul\0", generation: 7 }), /NUL/);
});

test("real Projects rendering retains local values and reports their declaration count", () => {
  function DraftView() {
    const state = useOwnerBrowser();
    return createElement(application.OwnerView, { state: {
      ...state, view: "projects", projectForm: { name: "<Literal> & project", repositories: "/first path\n/second", generation: 3 },
    } });
  }
  const markup = renderToStaticMarkup(createElement(DraftView));
  assert.match(markup, /value="&lt;Literal&gt; &amp; project"/);
  assert.match(markup, /2 declared repositories/);
  assert.match(markup, /\/first path\n\/second/);
  assert.match(markup, /<button\b[^>]*disabled=""[^>]*>Create project<\/button>/);
  assert.doesNotMatch(markup, /<Literal>/);
  assert.ok(Buffer.byteLength(markup) < 32768);
});
