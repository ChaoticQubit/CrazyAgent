import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-model-choice.ts");
const view = await import("../src/screens/settings/models-section.tsx");

const CATALOG = ["opus", "sonnet", "haiku"];
const candidates = (extra = []) => [...CATALOG.map((id) => ({ id, origin: "catalog" })), ...extra];
const EMPTY = { allowed: [], native: "follow" };
const row = (slot, extra = {}) => ({
  slot, here: null, effective: EMPTY[slot] ?? null, source: "default", version: 2, access: { kind: "editable" }, ...extra,
});
const page = (rows = {}, extra = {}) => ({
  models: {
    harness: "claude-code",
    rows: ["master", "agent", "allowed", "native"].map((slot) => row(slot, rows[slot])),
    candidates: candidates(extra.owned ?? []),
    qualified: extra.qualified ?? true,
    reason: extra.reason ?? null,
    live_note: extra.live_note ?? null,
    live_retry: extra.live_retry ?? false,
  },
  project_id: extra.project_id ?? null, qualified_versions: ["2.1.309"], sources: ["https://docs.anthropic.com/en/docs/claude-code/model-config"],
});
const section = (loaded, extra = {}) => renderToStaticMarkup(createElement(view.ModelsView, {
  page: loaded, name: "Claude Code", scope: "app", drafts: {}, busy: null, notes: {},
  onDraft: () => {}, onSave: () => {}, onReset: () => {}, onRetry: () => {}, ...extra,
}));
const ok = (value) => ({ kind: "ok", value });

test("every model choice code the engine can answer has its own sentence", () => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/model_choice.rs", import.meta.url), "utf8");
  const list = source.split("pub const MODEL_CHOICE_PROTOCOL_CODES: &[&str] = &[")[1].split("];")[0];
  const codes = [...list.matchAll(/"([a-z_]+)"/g)].map((match) => match[1]);
  assert(codes.length >= 20, "the engine's code list was read");
  for (const code of [...codes, "model_choice_stale"]) {
    assert(client.MODEL_CHOICE_REFUSALS.includes(code), `${code} has a sentence`);
    const words = client.modelChoiceRefusal({ code });
    assert.doesNotMatch(words, /\b[a-z]+_[a-z_]+\b/, `${code} reads as words, not a code`);
    assert.doesNotMatch(words, /\u2014/, "no em dash");
  }
  assert.equal(client.modelChoiceRefusal({ code: "model_choice_stale" }), client.MODEL_STALE_LINE);
  assert.equal(client.modelsNotReady({ kind: "error", code: "model_choice_unavailable", message: "" }), true);
  assert.equal(client.modelsNotReady({ kind: "not_ready" }), true);
  assert.equal(client.modelsStale({ kind: "error", code: "model_choice_stale", message: "" }), true);
  assert.equal(client.modelsStale({ kind: "ok", value: undefined }), false);
});

test("only catalog and tested ids can be chosen; pending and failed ids say why", () => {
  const owned = [{ id: "mine-a", origin: "pending" }, { id: "mine-b", origin: "verified" }, { id: "mine-c", origin: "failed" }];
  assert.deepEqual(client.usableIds(candidates(owned)), [...CATALOG, "mine-b"]);
  assert.equal(client.originWord("catalog"), "");
  assert.match(client.originWord("pending"), /test launch/);
  assert.match(client.originWord("failed"), /failed/);
  const all = candidates(owned);
  const draft = ["opus"];
  assert.equal(client.tickable(all[3], draft), false, "a pending id is never tickable");
  assert.equal(client.tickable(all[5], draft), false, "a failed id is never tickable");
  assert.equal(client.tickable(all[4], draft), true, "a tested id is tickable");
});

test("clearing the master or agent model is a reset, never a set of null", () => {
  const all = candidates();
  const unset = row("master");
  assert.equal(client.choiceDraft(unset, all), "");
  assert.deepEqual(client.choiceChange(unset, all, ""), { kind: "none" }, "nothing to clear");
  assert.deepEqual(client.choiceChange(unset, all, "opus"), { kind: "set", value: "opus" });
  const saved = row("master", { here: "opus", effective: "opus", source: "app" });
  assert.equal(client.choiceDraft(saved, all), "opus");
  assert.deepEqual(client.choiceChange(saved, all, "opus"), { kind: "none" });
  assert.deepEqual(client.choiceChange(saved, all, ""), { kind: "reset" });
  assert.deepEqual(client.choiceChange(saved, all, "sonnet"), { kind: "set", value: "sonnet" });
  assert.throws(() => client.choiceChange(saved, all, "not-offered"), /usable/);
  const app = client.choiceOptions(unset, all, "app");
  assert.deepEqual(app[0], { value: "", label: client.ACCOUNT_MODEL });
  assert.deepEqual(app.slice(1).map((option) => option.value), CATALOG);
  assert.equal(client.choiceOptions(unset, all, "project")[0].label, client.APP_VALUE);
  assert.equal(client.resetModelLabel(saved, "app"), "Use the account's own model");
  assert.equal(client.resetModelLabel(saved, "project"), "Use the app value");
});

test("an allowed list keeps candidate order, at most sixteen ids, and an own empty list saves", () => {
  const many = Array.from({ length: 20 }, (_, n) => ({ id: `m${n}`, origin: "catalog" }));
  let draft = [];
  for (const candidate of many.slice(0, client.ALLOWED_MAX)) draft = client.toggleAllowed(draft, candidate.id, true, many);
  assert.equal(draft.length, client.ALLOWED_MAX);
  assert.equal(client.tickable(many[16], draft), false, "the seventeenth id waits for one to be unticked");
  assert.equal(client.tickable(many[3], draft), true, "a ticked id can always be unticked");
  assert.throws(() => client.toggleAllowed(draft, "m17", true, many), /ALLOWED_MAX/);
  assert.deepEqual(client.toggleAllowed(["m5", "m1"], "m3", true, many), ["m1", "m3", "m5"]);
  assert.deepEqual(client.toggleAllowed(["m1", "m3"], "m1", false, many), ["m3"]);

  const all = candidates();
  const inherited = row("allowed", { effective: ["opus", "sonnet"], source: "app" });
  assert.deepEqual(client.allowedDraft(inherited), ["opus", "sonnet"]);
  assert.deepEqual(client.allowedChange(inherited, all, ["sonnet", "opus"]), { kind: "none" }, "the same ids in another order change nothing");
  assert.deepEqual(client.allowedChange(inherited, all, []), { kind: "set", value: [] }, "unticking the last id saves an own empty list");
  assert.deepEqual(client.allowedChange(inherited, all, ["haiku", "opus"]), { kind: "set", value: ["opus", "haiku"] });
  assert.equal(client.resetModelLabel(inherited, "app"), "Clear the list");
  assert.equal(client.resetModelLabel(inherited, "project"), "Use the app list");
});

test("a stored id the harness no longer offers says what runs instead and keeps its reset", () => {
  const all = candidates();
  const gone = row("master", { here: "retired-1", effective: null, source: "default" });
  assert.equal(client.choiceDraft(gone, all), "", "the choice starts on what runs");
  assert.equal(client.canResetModel(gone), true);
  assert.match(client.passedOverLine(gone, all), /Saved retired-1 isn't offered now, so Account's own model runs\./);
  const list = row("allowed", { here: ["opus", "retired-1", "retired-2"], effective: ["opus"], source: "app" });
  assert.match(client.passedOverLine(list, all), /Saved retired-1, retired-2 aren't offered now and are left out\./);
  assert.equal(client.passedOverLine(row("master", { here: "opus", effective: "opus", source: "app" }), all), "");
  assert.equal(client.canResetModel(row("agent")), false, "nothing stored, nothing to reset");
});

test("the Models section shows each slot with its control, source and catalog line", () => {
  const markup = section(ok(page({ master: { here: "opus", effective: "opus", source: "app" } })));
  assert.match(markup, />Models</);
  assert.match(markup, /Model list checked against Claude Code 2\.1\.309\./);
  for (const label of ["Master model", "Agent default model", "Allowed models"]) assert.match(markup, new RegExp(`>${label}<`));
  assert.match(markup, /<select id="models-master"/);
  assert.match(markup, /<select id="models-agent"/);
  assert.match(markup, /<option value="" selected="">Account&#x27;s own model<\/option>/, "the agent default starts on the account's model");
  assert.match(markup, /Set here for every project\./);
  assert.match(markup, /Nothing set: the account&#x27;s own model runs\./);
  assert.equal((markup.match(/type="checkbox"/g) ?? []).length, CATALOG.length, "one tick per candidate");
  assert.match(markup, /aria-label="Use the account&#x27;s own model, Master model"/, "a stored master value offers its reset");
  assert.doesNotMatch(markup, /aria-label="Save Master model"/, "no change, no Save");
  const changed = section(ok(page({ master: { here: "opus", effective: "opus", source: "app" } })), { drafts: { master: "sonnet" } });
  assert.match(changed, /aria-label="Save Master model"/);
});

test("a harness version the model list was not checked on shows the engine's reason, and the choices stay", () => {
  const reason = "listed for Claude Code 2.1.309, 2.1.322; installed 2.1.330 not yet checked";
  const unchecked = page({}, { qualified: false, reason });
  assert.equal(client.uncheckedLine(unchecked.models), `Version not checked: ${reason}.`);
  assert.equal(client.uncheckedLine(page().models), "");
  const markup = section(ok(unchecked));
  assert.match(markup, /Version not checked: listed for Claude Code 2\.1\.309, 2\.1\.322; installed 2\.1\.330 not yet checked\./);
  assert.match(markup, /<select id="models-master"/, "the ids can still be chosen");
  assert.doesNotMatch(section(ok(page())), /Version not checked/);
});

test("pending and failed owner ids show their state and stay unticked and disabled", () => {
  const owned = [{ id: "mine-a", origin: "pending" }, { id: "mine-c", origin: "failed" }];
  const markup = section(ok(page({}, { owned })));
  assert.match(markup, /mine-a <span class="small muted">Waiting for its test launch<\/span>/);
  assert.match(markup, /mine-c <span class="small muted">Test launch failed<\/span>/);
  assert.equal((markup.match(/type="checkbox" disabled=""/g) ?? []).length, 2, "both owner ids are disabled");
  assert.doesNotMatch(markup, /<option value="mine-a"/, "a pending id is not a choice");
});

test("a harness whose model CrazyAgent does not choose shows the reason and no control", () => {
  const unsupported = { access: { kind: "read_only", reason: "Not supported by this harness" }, source: "not_applied" };
  const markup = section(ok({ ...page({ master: unsupported, agent: unsupported, allowed: unsupported, native: unsupported }), qualified_versions: [], sources: [] }), { name: "OpenCode" });
  assert.equal((markup.match(/Not supported by this harness/g) ?? []).length >= 4, true);
  assert.doesNotMatch(markup, /<select|type="checkbox"|aria-label="Save /);
  assert.match(markup, /CrazyAgent has no model list for OpenCode\./);
  const stored = section(ok(page({ master: { ...unsupported, here: "opus" } })));
  assert.match(stored, /aria-label="Clear saved value, Master model"/, "a value stored before stays clearable");
});

test("a project page offers the app value, keeps inherit and own lists apart, and has no add or rules", () => {
  const inherit = page({ master: { effective: "opus", source: "app" }, allowed: { effective: ["opus"], source: "app" } }, { project_id: "p1" });
  const markup = section(ok(inherit), { scope: "project" });
  assert.match(markup, /<option value="" selected="">Use the app value<\/option>/);
  assert.match(markup, /Uses the app value: opus\./);
  assert.doesNotMatch(markup, /aria-label="Use the app value, Master model"/, "nothing stored here, nothing to clear");
  const own = page({ allowed: { here: [], effective: [], source: "project" } }, { project_id: "p1" });
  const ownMarkup = section(ok(own), { scope: "project" });
  assert.match(ownMarkup, /Set for this project\./);
  assert.match(ownMarkup, /aria-label="Use the app list, Allowed models"/);
  assert.match(ownMarkup, /None ticked: only the agent default runs\./);
  assert.throws(() => section(ok(page()), { scope: "project" }), /a project page names its project/);
});

test("the Models section says when it is loading, not ready or refused", () => {
  assert.match(section({ kind: "loading" }), /skeleton/);
  assert.match(section({ kind: "not_ready" }), /Model choice/);
  assert.match(section({ kind: "error", code: "model_choice_unavailable", message: "" }), /Model choice/);
  assert.match(section({ kind: "error", code: "model_choice_project_unknown", message: "" }), /This project is no longer in CrazyAgent\./);
});

test("your model ids lists each owner id with its state and a remove, and adds one", () => {
  const owned = [{ id: "mine-a", origin: "pending" }, { id: "mine-b", origin: "verified" }];
  const markup = renderToStaticMarkup(createElement(view.OwnIdsView, {
    candidates: candidates(owned), field: "", busy: null, note: "", onField: () => {}, onAdd: () => {}, onRemove: () => {}, testable: true,
  }));
  assert.match(markup, />Your model ids</);
  assert.match(markup, /<input id="models-add-id"/);
  assert.match(markup, /aria-label="Remove mine-a"/);
  assert.match(markup, /aria-label="Remove mine-b"/);
  assert.doesNotMatch(markup, /aria-label="Remove opus"/, "catalog ids cannot be removed");
  assert.match(markup, /<button[^>]*disabled=""[^>]*>Add</, "an empty field adds nothing");
  assert.match(client.addedLine("pending", "mine-a"), /waits for its test launch/);
  assert.match(client.addedLine("listed", "opus"), /already on this harness&#x27;s model list|already on this harness's model list/);
  assert.match(client.addedLine("verified", "mine-b"), /already passed its test launch/);
  assert.match(client.removedLine(true, "mine-a"), /removed/);
  assert.match(client.removedLine(false, "mine-a"), /already gone/);
});

test("the model rules show both rules with every option and the one in force", () => {
  const rules = { pick: "hint", pick_version: 3, fallback: "stop", fallback_version: 1 };
  const markup = renderToStaticMarkup(createElement(view.ModelRulesView, { rules: ok(rules), busy: null, note: "", onRule: () => {}, onRetry: () => {} }));
  assert.match(markup, />Model rules</);
  for (const option of [...client.PICK_OPTIONS, ...client.FALLBACK_OPTIONS]) assert.match(markup, new RegExp(`>${option.label}<`));
  assert.match(markup, /aria-checked="true"[^>]*>Any offered model</);
  assert.match(markup, /aria-checked="true"[^>]*>Stop and say why</);
  assert.match(markup, new RegExp(client.PICK_HELP.hint.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/'/g, "&#x27;")));
  assert.deepEqual(client.PICK_OPTIONS.map((option) => option.value), ["strict", "hint", "default"]);
  assert.deepEqual(client.FALLBACK_OPTIONS.map((option) => option.value), ["notice", "stop", "silent"]);
});

test("every call names its command once with exactly the window's arguments", () => {
  const source = readFileSync(new URL("../src/api-model-choice.ts", import.meta.url), "utf8");
  assert.match(source, /call<ModelsPage>\("desktop_model_choice", \{ harness, projectId, fresh, retry \}\)/);
  assert.match(source, /call<void>\("desktop_set_model_choice", \{ harness, projectId, slot, value, version \}\)/);
  assert.match(source, /call<void>\("desktop_reset_model_choice", \{ harness, projectId, slot, version \}\)/);
  assert.match(source, /call<ModelAdded>\("desktop_add_model", \{ harness, model, provider \}\)/);
  assert.match(source, /call<boolean>\("desktop_remove_model", \{ harness, model \}\)/);
  assert.match(source, /call<ModelRules>\("desktop_model_rules"\)/);
  assert.match(source, /call<void>\("desktop_set_model_rule", \{ rule, value, version \}\)/);
});

test("the Models section mounts once on the app and the project harness page", () => {
  const app = readFileSync(new URL("../src/screens/settings/harnesses.tsx", import.meta.url), "utf8");
  const project = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.equal((app.match(/<ModelsSection harness=\{entry\.id\} name=\{entry\.name\} projectId=\{null\} \/>/g) ?? []).length, 1);
  assert.equal((project.match(/<ModelsSection key=\{current\.id\} harness=\{current\.id\} name=\{current\.name\} projectId=\{data\.id\} \/>/g) ?? []).length, 1);
});

const FOLLOW = "Follow the agent's model";
const HARNESS_PICKS = "Let the harness choose";

test("the Native sub-agent model offers both options and starts on Follow the agent's model", () => {
  assert.deepEqual(client.NATIVE_OPTIONS, [
    { value: "follow", label: FOLLOW },
    { value: "harness", label: HARNESS_PICKS },
  ]);
  assert.equal(client.SLOT_LABELS.native, "Native sub-agent model");
  assert.equal(client.SLOT_HELP.native, "What the sub-agents a native lead starts itself run on.");
  assert.match(client.NATIVE_HELP.follow, /lead's model/);
  assert.match(client.NATIVE_HELP.harness, /allowed list/);
  const fresh = row("native");
  assert.equal(client.nativeDraft(fresh, "app"), "follow");
  assert.deepEqual(client.nativeOptions("app"), client.NATIVE_OPTIONS);
  assert.equal(client.modelSourceLine(fresh, "app"), "Nothing set: native sub-agents follow the agent's model.");
  assert.equal(client.canResetModel(fresh), false);
});

test("Follow the agent's model saves as its own choice after Let the harness choose", () => {
  const saved = row("native", { here: "harness", effective: "harness", source: "app" });
  assert.equal(client.nativeDraft(saved, "app"), "harness");
  assert.deepEqual(client.nativeChange(saved, "follow", "app"), { kind: "set", value: "follow" });
  assert.deepEqual(client.nativeChange(saved, "harness", "app"), { kind: "none" });
  assert.equal(client.resetModelLabel(saved, "app"), "Use the default");
  assert.equal(client.passedOverLine(saved, candidates()), "");
});

test("Let the harness choose saves from a fresh app page", () => {
  const fresh = row("native");
  assert.deepEqual(client.nativeChange(fresh, "harness", "app"), { kind: "set", value: "harness" });
  assert.deepEqual(client.nativeChange(fresh, "follow", "app"), { kind: "none" }, "Follow is already in force");
  assert.equal(client.valueText("harness"), "harness", "a model id stays as it is");
  assert.equal(client.nativeText("harness"), HARNESS_PICKS);
  assert.equal(client.nativeText("follow"), FOLLOW);
});

test("a project page's Native sub-agent model offers the app value, and a project choice clears back to it", () => {
  const inherit = row("native", { effective: "harness", source: "app" });
  assert.deepEqual(client.nativeOptions("project"), [{ value: "", label: client.APP_VALUE }, ...client.NATIVE_OPTIONS]);
  assert.equal(client.nativeDraft(inherit, "project"), "");
  assert.equal(client.modelSourceLine(inherit, "project"), `Uses the app value: ${HARNESS_PICKS}.`);
  assert.deepEqual(client.nativeChange(inherit, "follow", "project"), { kind: "set", value: "follow" });
  const own = row("native", { here: "follow", effective: "follow", source: "project" });
  assert.deepEqual(client.nativeChange(own, "", "project"), { kind: "reset" });
  assert.equal(client.resetModelLabel(own, "project"), client.APP_VALUE);
});

test("the Models section shows the Native sub-agent model with both options and the help of the one in force", () => {
  const markup = section(ok(page()));
  assert.match(markup, />Native sub-agent model</);
  assert.match(markup, /<select id="models-native"/);
  assert.match(markup, /<option value="follow" selected="">Follow the agent&#x27;s model<\/option>/);
  assert.match(markup, /<option value="harness">Let the harness choose<\/option>/);
  assert.match(markup, /Nothing set: native sub-agents follow the agent&#x27;s model\./);
  assert.doesNotMatch(markup, /aria-label="Save Native sub-agent model"/);
  const changed = section(ok(page()), { drafts: { native: "harness" } });
  assert.match(changed, /aria-label="Save Native sub-agent model"/);
  assert.match(changed, /allowed list/, "the help of the picked option shows");
});

test("RCLI's Native sub-agent model is read-only with Not supported by this harness and no raw value", () => {
  const unsupported = { access: { kind: "read_only", reason: "Not supported by this harness" }, source: "not_applied" };
  const markup = section(ok({ ...page({ master: unsupported, agent: unsupported, allowed: unsupported, native: unsupported }), qualified_versions: [], sources: [] }), { name: "RCLI" });
  assert.match(markup, />Native sub-agent model</);
  assert.doesNotMatch(markup, /<select id="models-native"/);
  assert.doesNotMatch(markup, />follow</, "the stored word never shows");
});

test("the TUI Models page uses the desktop's Native sub-agent model words (DEC-141 parity)", () => {
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/settings/models_page.rs", import.meta.url), "utf8");
  for (const words of [
    client.SLOT_LABELS.native, client.SLOT_HELP.native, FOLLOW, HARNESS_PICKS,
    client.NATIVE_HELP.follow, client.NATIVE_HELP.harness,
    "Nothing set: native sub-agents follow the agent's model.",
  ]) {
    assert(tui.includes(JSON.stringify(words)), `the TUI says ${words}`);
  }
});

test("the engine's live list note shows word for word under the catalog line, and the ids stay", () => {
  const note = "live list not reported by Claude Code";
  const unreported = page({}, { live_note: note });
  assert.equal(client.liveLine(unreported.models), `${note}.`);
  assert.equal(client.liveLine(page().models), "");
  const markup = section(ok(unreported));
  assert.match(markup, /live list not reported by Claude Code\./);
  assert.match(markup, /<select id="models-master"/, "the shipped ids can still be chosen");
  assert.doesNotMatch(section(ok(page())), /live list not reported/);
});

test("Check again shows beside the live list note only when the engine can read the list again", () => {
  const note = "live list not reported by Claude Code";
  const markup = section(ok(page({}, { live_note: note, live_retry: true })));
  assert.match(markup, /live list not reported by Claude Code\.\s*<button[^>]*>Check again<\/button>/);
  assert.doesNotMatch(section(ok(page({}, { live_note: note }))), /Check again/, "no list command or no executable");
  assert.doesNotMatch(section(ok(page({}, { live_retry: true }))), /Check again/, "the list was reported");
});

test("the Models section reads the live list only when it opens and on Retry, never after a change", () => {
  const source = readFileSync(new URL("../src/screens/settings/models-section.tsx", import.meta.url), "utf8");
  assert.match(source, /loadModels\(harness, projectId, live, forced\)/, "the read names whether it is fresh and asked again");
  assert.match(source, /const live = fresh\.current;\s*fresh\.current = false;/, "only the first read after an open is fresh");
  assert.match(source, /const forced = again\.current;\s*again\.current = false;/, "only Retry and Check again skip the backoff");
  assert.match(source, /function retry\(\) \{\s*fresh\.current = true;\s*again\.current = true;\s*reload\(\);\s*\}/, "Retry reads it again past the backoff");
  assert.match(source, /onRetry=\{retry\}/);
  assert.doesNotMatch(source, /onRetry=\{reload\}/, "Retry never re-reads stale");
});
