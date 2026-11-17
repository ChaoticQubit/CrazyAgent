import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/team-look.mjs", import.meta.url).pathname);
const sheets = ["../src/screens/team/team-graph.css", "../src/screens/team/agent-messages.css"].map((path) => [path, readFileSync(new URL(path, import.meta.url), "utf8")]);

/**
 * Mounts the team graph parts in one theme and reads their look. Input: the theme.
 * Output: what `Fixture.look()` returns.
 */
function looks(theme) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: an open card, a live label, a count badge and the live and message lines are ink, with no accent`, async () => {
    const seen = await looks(theme);
    const { primary, text } = seen.tokens;
    assert.equal(seen.open.edge, primary);
    assert.ok(seen.open.halo.startsWith(`${seen.tokens["surface-3"]} 0px 0px 0px 3px`), seen.open.halo);
    assert.deepEqual([seen.badge.edge, seen.badge.color], [primary, text]);
    assert.deepEqual(seen.live, { edge: primary, fill: seen.tokens["surface-3"], color: text, weight: "500", dot: primary });
    assert.deepEqual(seen.lines, { live: primary, message: primary });
  });

  test(`${theme}: team graph type sits on the scale at 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual([seen.card.title.size, seen.card.title.weight, seen.card.detail, seen.card.branch], ["13px", "500", "12px", "12px"]);
    assert.deepEqual([seen.chip.size, seen.chip.weight], ["11px", "500"]);
    assert.deepEqual([seen.badge.size, seen.badge.weight], ["11px", "500"]);
    assert.deepEqual(seen.label, { size: "11px", line: "14px", weight: "500", mono: true });
    assert.deepEqual([seen.legend.size, seen.legend.line, seen.legend.mono], ["11px", "16px", true]);
  });

  test(`${theme}: an agent's Messages title and speakers sit at 500, its small lines at 12px, and a result keeps its ink bar`, async () => {
    const seen = await looks(theme);
    assert.deepEqual([seen.messages.title.size, seen.messages.title.weight], ["13px", "500"]);
    assert.deepEqual(seen.messages.speaker, { weight: "500", color: seen.tokens.text });
    assert.deepEqual([seen.messages.time, seen.messages.state, seen.messages.from], ["12px", "12px", "12px"]);
    assert.equal(seen.messages.bar, `3px ${seen.tokens.text}`);
  });
}

for (const [path, css] of sheets) {
  test(`${path.split("/").at(-1)} names no accent token, no weight 600 or 700 and no literal font size`, () => {
    assert.doesNotMatch(css, /var\(--(?:accent|on-accent)/);
    assert.doesNotMatch(css, /font-weight:\s*[67]00|font:\s*[67]00/);
    assert.doesNotMatch(css, /font-size:\s*[0-9.]+(?:px|rem|em)|\bfont:[^;]*\b[0-9.]+px/);
  });
}
