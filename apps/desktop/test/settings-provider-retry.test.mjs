import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const settings = await import("../src/screens/settings/provider-retry.tsx");
const harnesses = await import("../src/screens/settings/harnesses.tsx");

const LINES = {
  stop_at_once: [
    "Claude Code: no retries.",
    "Codex: no retries on an API key or address account. With a ChatGPT sign-in Codex takes no retry count, so it only stops waiting for the network without end. The Codex master keeps its own retries for now.",
    "RCLI: one try. The engine never repeats an RCLI request.",
  ],
  retry_briefly: [
    "Claude Code: its own retries, up to 10 with longer waits each time.",
    "Codex: its own retries, without waiting for the network without end. The Codex master keeps its own retries and that wait for now.",
    "RCLI: one try. The engine never repeats an RCLI request.",
  ],
  harness_default: [
    "Claude Code: its own retries, up to 10 with longer waits each time.",
    "Codex: its own retries. A provider that refuses connections keeps it waiting for the network without end.",
    "RCLI: one try. The engine never repeats an RCLI request.",
  ],
};
const HARNESSES = ["claude-code", "codex", "rcli"];
const notes = (lines = LINES) => Object.entries(lines)
  .flatMap(([option, shown]) => shown.map((line, index) => ({ option, harness: HARNESSES[index], line })));
const ok = (choice, lines = LINES) => ({ kind: "ok", value: { choice, notes: notes(lines) } });
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const view = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.ProviderRetrySettingsView, {
  retry: loaded, picked: null, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/\s+/gu, " ");
const checked = (markup) => [...markup.matchAll(/<input type="radio" name="provider-retry"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z_]+)"/)[1]);
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);

test("the section names the three options in order, each with what it does and the engine's line per harness", () => {
  assert.deepEqual(settings.PROVIDER_RETRY_CHOICES.map((option) => option.choice), ["stop_at_once", "retry_briefly", "harness_default"]);
  assert.equal(settings.PROVIDER_RETRY_DEFAULT, "retry_briefly");
  const markup = view(ok("retry_briefly"));
  const shown = text(markup);
  assert.match(shown, /When a model provider can't be reached/);
  assert.match(shown, /What harnesses do/);
  assert.match(shown, /New runs take the choice; runs already going keep theirs\. Each harness line is proven on read-only runs\./);
  const cards = [...markup.matchAll(/<label class="radio-card">([\s\S]*?)<\/label>/g)].map(([, card]) => text(card).trim());
  assert.equal(cards.length, 3);
  settings.PROVIDER_RETRY_CHOICES.forEach((option, index) => {
    assert.ok(cards[index].startsWith(`${option.title} ${option.summary}`), cards[index]);
    assert.ok(cards[index].endsWith(LINES[option.choice].join(" ")), cards[index]);
  });
  assert.deepEqual(settings.PROVIDER_RETRY_CHOICES.map((option) => option.title), ["Stop at once", "Retry briefly", "Harness default"]);
  assert.ok(!/—/u.test(shown));
});

test("the lines are the engine's words, not the app's", () => {
  const changed = { ...LINES, stop_at_once: ["Claude Code: engine words one.", "Codex: engine words two.", "RCLI: engine words three."] };
  const shown = text(view(ok("stop_at_once", changed)));
  for (const line of changed.stop_at_once) assert.ok(shown.includes(line), line);
  assert.ok(!shown.includes("Claude Code: no retries."));
});

test("Retry briefly is checked when stored, has no changed mark and Save waits for a change", () => {
  const markup = view(ok("retry_briefly"));
  assert.deepEqual(checked(markup), ["retry_briefly"]);
  assert.ok(!markup.includes("set-changed"));
  assert.ok(saveDisabled(markup));
});

test("Stop at once: picking it checks it, marks the line changed and enables Save", () => {
  const markup = view(ok("retry_briefly"), { picked: "stop_at_once" });
  assert.deepEqual(checked(markup), ["stop_at_once"]);
  assert.ok(markup.includes("set-changed"));
  assert.ok(!saveDisabled(markup));
  assert.deepEqual(checked(view(ok("stop_at_once"))), ["stop_at_once"]);
  assert.ok(saveDisabled(view(ok("stop_at_once"))));
});

test("Harness default: a stored choice is checked and marked changed; picking the stored one again leaves Save off", () => {
  const markup = view(ok("harness_default"));
  assert.deepEqual(checked(markup), ["harness_default"]);
  assert.ok(markup.includes("set-changed"));
  assert.ok(saveDisabled(view(ok("harness_default"), { picked: "harness_default" })));
  assert.ok(!saveDisabled(view(ok("harness_default"), { picked: "retry_briefly" })));
});

test("while saving the choices are locked and a refused save says why", () => {
  const saving = view(ok("retry_briefly"), { picked: "stop_at_once", saving: true });
  assert.match(saving, /<fieldset class="field" disabled="">/);
  assert.match(text(saving), /Saving\.\.\./);
  const refused = view(ok("retry_briefly"), { picked: "stop_at_once", problem: "The engine refused the setting." });
  assert.match(refused, /role="alert"/);
  assert.match(text(refused), /The engine refused the setting\./);
});

test("an engine without the setting shows nothing, a failed read says so with Try again", () => {
  assert.equal(view({ kind: "loading" }), "");
  assert.equal(view({ kind: "not_ready" }), "");
  const failed = text(view({ kind: "error", message: "database is locked" }));
  assert.match(failed, /Couldn't load the setting for model providers that can't be reached\. database is locked/);
  assert.match(failed, /Try again/);
});

test("the Harnesses page shows the section after its update settings and before the harness groups", () => {
  const catalog = { kind: "ok", value: [{ id: "codex", name: "Codex", status: { kind: "ready" }, status_label: "", executable: null, headless: "codex exec",
    tools: { kind: "switchable" }, model_selection: true, account_kinds: ["subscription"] }] };
  const page = text(renderToStaticMarkup(createElement(harnesses.HarnessesView, {
    catalog, onCheck: forbidden, retry: createElement(settings.ProviderRetrySettingsView, {
      retry: ok("retry_briefly"), picked: null, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden,
    }),
  })));
  const section = page.indexOf("When a model provider can't be reached");
  assert.ok(section > page.indexOf("Check again"), page);
  assert.ok(section < page.indexOf("Ready (1)"), page);
});
