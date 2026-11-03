import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { fileURLToPath } from "node:url";
import test from "node:test";
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
  assert.equal(displayText(JSON.stringify({ name: "Project\u202e", repositories: ["/repo\u2066"] })),
    '{"name":"Project\\u202e","repositories":["/repo\\u2066"]}');
});
