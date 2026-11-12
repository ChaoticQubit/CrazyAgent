import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const settings = await import("../src/api-ticket-settings.ts");
const { TicketSettingsView, saveNote } = await import("../src/screens/settings/tickets.tsx");
const roadmap = await import("../src/screens/project/roadmap.tsx");
const { SETTINGS_TABS, PROJECT_TABS } = await import("../src/ui/router.ts");
const { failureCopy } = await import("../src/api-projects.ts");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const markup = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("\"", "&quot;").replaceAll("'", "&#x27;");
const forbidden = () => { throw new Error("static rendering must not act"); };
const radios = (html) => (html.match(/type="radio"/g) ?? []).length;

const OPTIONS = {
  close: ["agents", "evidence", "owner"],
  create: ["direct", "triage", "owner_only"],
  agent_edit: ["any", "status_and_criteria", "none"],
  cycle_days: [7, 14, 21, 28],
  rollover: ["auto", "ask", "off"],
  import: ["auto_once", "on_request"],
  manual_evidence: ["owner", "agents"],
  ci_evidence: ["engine", "owner", "agents"],
};
const PROPOSED = { close: "evidence", create: "triage", agent_edit: "status_and_criteria", cycle_days: 14, rollover: "auto", import: "auto_once", manual_evidence: "owner", ci_evidence: "engine" };
const rowOf = (key, here = null, below = PROPOSED[key]) => ({
  key, options: OPTIONS[key], proposed: PROPOSED[key], custom: key === "cycle_days" ? { min: 1, max: 90 } : null, here, below, effective: here ?? below,
});
const pageOf = (projectId, here = {}, below = {}) => ({
  project_id: projectId, version: 4, keys: settings.TICKET_KEYS.map((key) => rowOf(key, here[key] ?? null, below[key] ?? PROPOSED[key])),
});
const view = (page, scope, drafts = {}) => renderToStaticMarkup(createElement(TicketSettingsView, {
  page, scope, projectId: scope === "app" ? null : "project-a", drafts, busy: false, note: "",
  onDraft: forbidden, onSave: forbidden, onUseAppDefault: forbidden, onRetry: forbidden,
}));

const cycle = (number, status, starts_day, ends_day, extra = {}) => ({ number, status, starts_day, ends_day, release: null, version: 1, closed: 0, total: 0, ...extra });
const version = (name, position, target_day, extra = {}) => ({ name, position, target_day, status: "planned", version: 1, closed: 0, total: 0, ...extra });
const board = (cycles, versions = []) => ({ unplanned: { closed: 1, total: 2 }, versions, cycles });
const roadmapView = (props) => renderToStaticMarkup(createElement(roadmap.RoadmapView, {
  today: "2026-11-03", rollover: null, closed: null, preview: null, skipped: new Set(), busy: false, note: "",
  onStart: forbidden, onClose: forbidden, onMove: forbidden, onPreview: forbidden, onSkip: forbidden, onApply: forbidden, onCancelImport: forbidden,
  ...props,
}));

test("every_policy_option_is_listed_with_the_default_labelled", () => {
  assert.deepEqual(settings.TICKET_KEYS, Object.keys(OPTIONS));
  const app = view({ kind: "ok", value: pageOf(null) }, "app");
  const project = view({ kind: "ok", value: pageOf("project-a", {}, { close: "owner" }) }, "project");
  for (const key of settings.TICKET_KEYS) {
    const text = settings.keyText(key);
    assert.ok(app.includes(markup(text.label)), key);
    assert.ok(app.includes(markup(text.help)), key);
    for (const option of OPTIONS[key]) {
      const words = settings.optionText(key, option);
      assert.ok(words.label.length > 0 && words.line.length > 0, `${key} ${option}`);
      assert.ok(app.includes(markup(words.label)), `${key} ${option} label`);
      assert.ok(app.includes(markup(words.line)), `${key} ${option} line`);
    }
    const proposed = settings.optionText(key, PROPOSED[key]).label;
    assert.ok(app.includes(markup(`Default (${proposed})`)), `${key} default`);
  }
  assert.ok(project.includes(markup("App default (Only you)")));
  assert.ok(project.includes(markup("App default (Through Triage)")));
  assert.ok(!project.includes(markup("App default (When the evidence is in)")));
  assert.ok(!project.includes("class=\"title\">Default ("));
  assert.ok(!app.includes("App default ("));
  const optionCount = Object.values(OPTIONS).reduce((sum, options) => sum + options.length, 0);
  assert.equal(radios(app), settings.TICKET_KEYS.length + optionCount + 1);
  assert.equal((app.match(/>Custom</g) ?? []).length, 1);
  assert.ok(app.includes(markup("Pick any length from 1 to 90 days.")));
  assert.ok(app.includes(markup(settings.TICKET_EFFECT_LINE)));
  assert.ok(project.includes(markup("A value set here wins over the app value for this project only.")));
  assert.deepEqual([7, 14, 21, 28, 10, 1].map((days) => settings.optionText("cycle_days", days).label), ["1 week", "2 weeks", "3 weeks", "4 weeks", "10 days", "1 day"]);
  assert.equal(settings.optionText("cycle_days", 1).line, "Each new cycle runs 1 day.");
  assert.equal(settings.optionText("cycle_days", 10).line, "Each new cycle runs 10 days.");
  assert.throws(() => settings.optionText("close", "sometimes"), /close has no option sometimes/);
  assert.throws(() => settings.optionText("cycle_days", "14"), /whole number of days/);
});

test("roadmap progress counts closed of total, rounded down, and refuses impossible counts", () => {
  assert.deepEqual(roadmap.progress(0, 0), { percent: 0, line: "No tickets yet" });
  assert.deepEqual(roadmap.progress(0, 4), { percent: 0, line: "0 of 4 done (0%)" });
  assert.deepEqual(roadmap.progress(1, 3), { percent: 33, line: "1 of 3 done (33%)" });
  assert.deepEqual(roadmap.progress(2, 3), { percent: 66, line: "2 of 3 done (66%)" });
  assert.deepEqual(roadmap.progress(199, 200), { percent: 99, line: "199 of 200 done (99%)" });
  assert.deepEqual(roadmap.progress(3, 3), { percent: 100, line: "3 of 3 done (100%)" });
  for (const [closed, total] of [[4, 3], [-1, 2], [1.5, 2], [1, Number.NaN]]) assert.throws(() => roadmap.progress(closed, total), `${closed}/${total}`);
  const html = roadmapView({ roadmap: board([cycle(3, "active", "2026-11-02", "2026-11-15", { closed: 1, total: 3 })], [version("1.0", 0, "2026-12-01", { closed: 2, total: 3 })]) });
  assert.ok(html.includes("aria-valuenow=\"33\""));
  assert.ok(html.includes("aria-valuenow=\"66\""));
  assert.ok(html.includes("Not in a version: 1 of 2 done (50%)"));
});

test("cycle days left count today and the last day, and target days read against today", () => {
  const sprint = cycle(3, "active", "2026-11-02", "2026-11-15");
  assert.equal(roadmap.cycleTime(sprint, "2026-11-02"), "14 days left");
  assert.equal(roadmap.cycleTime(sprint, "2026-11-03"), "13 days left");
  assert.equal(roadmap.cycleTime(sprint, "2026-11-14"), "2 days left");
  assert.equal(roadmap.cycleTime(sprint, "2026-11-15"), "Last day");
  assert.equal(roadmap.cycleTime(sprint, "2026-11-16"), "Ended yesterday");
  assert.equal(roadmap.cycleTime(sprint, "2026-11-18"), "Ended 3 days ago");
  assert.equal(roadmap.cycleTime(cycle(4, "upcoming", "2026-11-04", "2026-11-17"), "2026-11-03"), "Starts tomorrow");
  assert.equal(roadmap.cycleTime(cycle(4, "upcoming", "2026-11-10", "2026-11-23"), "2026-11-03"), "Starts in 7 days");
  assert.equal(roadmap.cycleTime(cycle(2, "closed", "2026-10-19", "2026-11-01"), "2026-11-03"), "Closed, ran 14 days");
  assert.equal(roadmap.cycleTime(cycle(9, "active", "2028-02-28", "2028-03-01"), "2028-02-28"), "3 days left");
  assert.throws(() => roadmap.cycleTime(cycle(5, "active", "2026-11-15", "2026-11-02"), "2026-11-03"), /ends on or after/);
  assert.equal(roadmap.dayNumber("1970-01-02"), 1);
  for (const bad of ["2026-02-29", "2026-13-01", "2026-11-31", "26-01-01", "2026-1-01", ""]) assert.equal(roadmap.dayNumber(bad), null, bad);
  assert.equal(roadmap.dayNumber("2028-02-29") - roadmap.dayNumber("2028-02-28"), 1);
  assert.equal(roadmap.todayText(new Date(2026, 0, 5, 23, 59)), "2026-01-05");
  assert.equal(roadmap.targetLine({ status: "released", target_day: "2026-01-01" }, "2026-11-03"), "Released");
  assert.equal(roadmap.targetLine({ status: "planned", target_day: null }, "2026-11-03"), "No target day");
  assert.equal(roadmap.targetLine({ status: "active", target_day: "2026-11-03" }, "2026-11-03"), "Due today, 2026-11-03");
  assert.equal(roadmap.targetLine({ status: "planned", target_day: "2026-11-04" }, "2026-11-03"), "Due in 1 day, 2026-11-04");
  assert.equal(roadmap.targetLine({ status: "planned", target_day: "2026-12-01" }, "2026-11-03"), "Due in 28 days, 2026-12-01");
  assert.equal(roadmap.targetLine({ status: "planned", target_day: "2026-11-02" }, "2026-11-03"), "1 day past 2026-11-02");
  assert.equal(roadmap.targetLine({ status: "planned", target_day: "2026-11-01" }, "2026-11-03"), "2 days past 2026-11-01");
  const html = roadmapView({ roadmap: board([sprint, cycle(2, "closed", "2026-10-19", "2026-11-01")]) });
  assert.ok(html.includes("Cycle 3"));
  assert.ok(html.includes("2026-11-02 to 2026-11-15, 13 days left"));
  assert.ok(html.includes("Other cycles (1)"));
  assert.ok(html.includes("Closed, ran 14 days"));
  assert.ok(html.includes("Close cycle 3"));
  assert.ok(html.includes("Plan the next cycle"));
  const idle = roadmapView({ roadmap: board([]) });
  assert.ok(idle.includes("No cycle is running."));
  assert.ok(idle.includes("Start a cycle"));
  assert.ok(!idle.includes("Close cycle"));
});

test("each rollover option says what closing does, and a close says what it did", () => {
  assert.deepEqual(OPTIONS.rollover.map(roadmap.rolloverLine), [
    "Unfinished tickets move to the next cycle, which starts if none is open.",
    "You'll move each unfinished ticket yourself after the cycle closes.",
    "Unfinished tickets stay in the closed cycle.",
  ]);
  const active = board([cycle(3, "active", "2026-11-02", "2026-11-15")]);
  for (const rollover of OPTIONS.rollover) assert.ok(roadmapView({ roadmap: active, rollover }).includes(markup(roadmap.rolloverLine(rollover))), rollover);
  assert.ok(!roadmapView({ roadmap: active }).includes(markup(roadmap.rolloverLine("auto"))));
  const closed = (rollover, unfinished, moved_to, tickets) => ({ number: 3, rollover, unfinished, moved_to, tickets });
  assert.equal(roadmap.closedLine(closed("auto", 0, null, [])), "Cycle 3 closed. Every ticket in it was done.");
  assert.equal(roadmap.closedLine(closed("auto", 2, 4, ["RB-4", "RB-7"])), "Cycle 3 closed. 2 unfinished tickets moved to cycle 4.");
  assert.equal(roadmap.closedLine(closed("ask", 1, null, ["RB-4"])), "Cycle 3 closed. 1 unfinished ticket waits for you: move it below.");
  assert.equal(roadmap.closedLine(closed("ask", 2, null, ["RB-4", "RB-7"])), "Cycle 3 closed. 2 unfinished tickets wait for you: move each one below.");
  assert.equal(roadmap.closedLine(closed("off", 1, null, [])), "Cycle 3 closed. 1 unfinished ticket stays in it.");
  assert.equal(roadmap.closedLine(closed("off", 2, null, [])), "Cycle 3 closed. 2 unfinished tickets stay in it.");
  assert.throws(() => roadmap.closedLine(closed("auto", 2, null, [])), /names the cycle/);
  assert.throws(() => roadmap.closedLine(closed("ask", 1, null, ["RB-4", "RB-7"])), /unfinished ones/);

  const reply = { closed: { number: 3, status: "closed" }, rollover: "ask", unfinished: 2, moved_to: null, tickets: ["RB-4", "RB-7"] };
  assert.deepEqual(roadmap.cycleClosed(reply), { number: 3, rollover: "ask", unfinished: 2, moved_to: null, tickets: ["RB-4", "RB-7"] });
  assert.equal(roadmap.cycleClosed({ ...reply, rollover: "later" }), null);
  assert.equal(roadmap.cycleClosed({ ...reply, rollover: "auto" }), null);
  assert.equal(roadmap.cycleClosed({ ...reply, tickets: ["RB-4", "RB-7", "RB-9"] }), null);
  assert.equal(roadmap.cycleClosed({ ...reply, tickets: [4] }), null);
  assert.equal(roadmap.cycleClosed({ ...reply, unfinished: -1 }), null);

  const cycles = [cycle(1, "closed", "2026-10-05", "2026-10-18"), cycle(3, "closed", "2026-11-02", "2026-11-15"), cycle(5, "upcoming", "2026-11-30", "2026-12-13"), cycle(4, "upcoming", "2026-11-16", "2026-11-29")];
  assert.equal(roadmap.moveTarget(cycles, 3), 4);
  assert.equal(roadmap.moveTarget(cycles, 5), null);
  assert.equal(roadmap.moveTarget(cycles.slice(0, 2), 3), null);
  const asked = roadmap.cycleClosed(reply);
  const moving = roadmapView({ roadmap: board(cycles), closed: asked });
  assert.equal((moving.match(/Move to cycle 4/g) ?? []).length, 2);
  assert.ok(moving.includes(markup(roadmap.closedLine(asked))));
  const stuck = roadmapView({ roadmap: board(cycles.slice(0, 2)), closed: asked });
  assert.equal((stuck.match(/Start the next cycle to move it\./g) ?? []).length, 2);
  const auto = roadmapView({ roadmap: board(cycles), closed: { ...asked, rollover: "auto", moved_to: 4 } });
  assert.ok(!auto.includes("Move to cycle"));
});

test("an import shows every change to accept or skip, and what it leaves to a person", () => {
  const preview = {
    items: [
      { key: "v:1.0", change: { change: "version_add", name: "1.0", position: 0, target_day: "2026-12-01" } },
      { key: "v:1.1", change: { change: "version_change", name: "1.1", position: 1, target_day: null, was_position: 0, was_target_day: "2026-12-01" } },
      { key: "t:3", change: { change: "ticket_add", version: "1.0", kind: "feature", title: "Share a recipe" } },
    ],
    unparsed: [
      { heading: "Ideas", line_first: 3, line_last: 3, why: "not_a_version", text: "## Ideas" },
      { heading: "", line_first: 1, line_last: 2, why: "before_first_section", text: "Intro" },
    ],
  };
  assert.deepEqual(roadmap.importPreview(preview), preview);
  assert.deepEqual(roadmap.acceptedKeys(preview, new Set()), ["v:1.0", "v:1.1", "t:3"]);
  assert.deepEqual(roadmap.acceptedKeys(preview, new Set(["v:1.1"])), ["v:1.0", "t:3"]);
  assert.deepEqual(roadmap.acceptedKeys(preview, new Set(["v:1.0", "v:1.1", "t:3"])), []);
  assert.throws(() => roadmap.acceptedKeys({ ...preview, items: [preview.items[0], preview.items[0]] }, new Set()), /its own key/);
  assert.deepEqual(preview.items.map((item) => roadmap.importLine(item.change)), [
    "Add version 1.0 at place 1, 2026-12-01",
    "Change version 1.1: place 1 to 2, 2026-12-01 to no target",
    "New feature in 1.0: Share a recipe",
  ]);
  assert.equal(roadmap.importLine({ change: "ticket_add", version: null, kind: "bug", title: "Fix login" }), "New bug: Fix login");
  assert.equal(roadmap.importLine({ change: "version_change", name: "2.0", position: 2, target_day: "2027-01-04", was_position: 2, was_target_day: null }), "Change version 2.0: no target to 2027-01-04");
  assert.throws(() => roadmap.importLine({ change: "version_change", name: "2.0", position: 2, target_day: null, was_position: 2, was_target_day: null }), /place or target/);
  assert.equal(roadmap.unparsedLine(preview.unparsed[0]), "Line 3 (Ideas): the heading names no version");
  assert.equal(roadmap.unparsedLine(preview.unparsed[1]), "Lines 1 to 2: text before the first version heading");
  assert.equal(roadmap.unparsedLine({ ...preview.unparsed[0], why: "new_reason" }), "Line 3 (Ideas): not imported");
  assert.equal(roadmap.importPreview({ ...preview, items: [{ key: "x", change: { change: "version_drop" } }] }), null);
  assert.equal(roadmap.importPreview({ ...preview, items: Array.from({ length: 257 }, (_, at) => ({ ...preview.items[2], key: `t:${at}` })) }), null);
  assert.equal(roadmap.importPreview({ items: [], unparsed: [{ heading: 1 }] }), null);

  const empty = board([]);
  const before = roadmapView({ roadmap: empty });
  assert.ok(before.includes(markup(`Reads ${roadmap.ROADMAP_PATH} and shows what would change before anything does.`)));
  const all = roadmapView({ roadmap: empty, preview });
  assert.equal((all.match(/type="checkbox" checked=""/g) ?? []).length, 3);
  assert.ok(all.includes("Apply 3 changes"));
  for (const item of preview.items) assert.ok(all.includes(markup(roadmap.importLine(item.change))));
  for (const part of preview.unparsed) assert.ok(all.includes(markup(roadmap.unparsedLine(part))));
  const some = roadmapView({ roadmap: empty, preview, skipped: new Set(["v:1.0", "t:3"]) });
  assert.equal((some.match(/type="checkbox" checked=""/g) ?? []).length, 1);
  assert.ok(some.includes("Apply 1 change<"));
  const none = roadmapView({ roadmap: empty, preview, skipped: new Set(["v:1.0", "v:1.1", "t:3"]) });
  assert.match(none, /<button[^>]*disabled=""[^>]*>Apply 0 changes<\/button>/);
  const nothing = roadmapView({ roadmap: empty, preview: { items: [], unparsed: [] } });
  assert.ok(nothing.includes(markup("The roadmap matches what's here. Nothing to import.")));
});

test("a project override sets keys, keeps the app value below and clears with Use app default", () => {
  assert.deepEqual(settings.draftOf(rowOf("close")), { kind: "below" });
  assert.deepEqual(settings.draftOf(rowOf("close", "owner")), { kind: "option", value: "owner" });
  assert.deepEqual(settings.draftOf(rowOf("cycle_days", 21)), { kind: "option", value: 21 });
  assert.deepEqual(settings.draftOf(rowOf("cycle_days", 10)), { kind: "custom", text: "10" });
  const days = rowOf("cycle_days");
  assert.deepEqual(settings.draftValue(days, { kind: "custom", text: "10" }), { value: 10 });
  assert.deepEqual(settings.draftValue(days, { kind: "custom", text: " 90 " }), { value: 90 });
  assert.deepEqual(settings.draftValue(days, { kind: "custom", text: "1" }), { value: 1 });
  for (const bad of ["", "0", "91", "1.5", "-3", "two", "000000001"]) {
    assert.deepEqual(settings.draftValue(days, { kind: "custom", text: bad }), { problem: "Use a whole number of days from 1 to 90." }, bad);
  }
  assert.throws(() => settings.draftValue(rowOf("close"), { kind: "custom", text: "3" }), /custom number/);
  assert.throws(() => settings.draftValue(rowOf("close"), { kind: "option", value: "sometimes" }), /one of the row/);

  const fresh = pageOf("project-a", {}, { close: "owner", cycle_days: 10 });
  assert.deepEqual(settings.valuesOf(fresh.keys, {}), { values: {} });
  assert.equal(settings.draftsChanged(fresh.keys, {}), false);
  assert.equal(settings.canUseAppDefault(fresh), false);
  const drafts = { close: { kind: "option", value: "agents" }, cycle_days: { kind: "custom", text: "12" }, rollover: { kind: "below" } };
  assert.deepEqual(settings.valuesOf(fresh.keys, drafts), { values: { close: "agents", cycle_days: 12 } });
  assert.equal(settings.draftsChanged(fresh.keys, drafts), true);
  assert.deepEqual(settings.valuesOf(fresh.keys, { cycle_days: { kind: "custom", text: "120" } }), { key: "cycle_days", problem: "Use a whole number of days from 1 to 90." });
  assert.equal(settings.draftsChanged(fresh.keys, { cycle_days: { kind: "custom", text: "120" } }), true);

  const set = pageOf("project-a", { close: "agents", rollover: "off" }, { close: "owner" });
  assert.equal(settings.canUseAppDefault(set), true);
  assert.deepEqual(settings.valuesOf(set.keys, {}), { values: { close: "agents", rollover: "off" } });
  assert.deepEqual(settings.valuesOf(set.keys, { close: { kind: "below" } }), { values: { rollover: "off" } });
  assert.equal(settings.draftsChanged(set.keys, { close: { kind: "option", value: "agents" } }), false);
  assert.equal(settings.draftsChanged(set.keys, { close: { kind: "below" } }), true);
  assert.equal(settings.canUseAppDefault(pageOf(null, { close: "agents" })), false);

  const shown = view({ kind: "ok", value: set }, "project");
  assert.ok(shown.includes(">Use app default<"));
  assert.ok(!shown.includes(">Save<"));
  assert.ok(view({ kind: "ok", value: set }, "project", { close: { kind: "below" } }).includes(">Save<"));
  assert.ok(!view({ kind: "ok", value: fresh }, "project").includes(">Use app default<"));
  assert.ok(!view({ kind: "ok", value: pageOf(null, { close: "agents" }) }, "app").includes(">Use app default<"));
  const custom = view({ kind: "ok", value: fresh }, "project", { cycle_days: { kind: "custom", text: "120" } });
  assert.ok(custom.includes("id=\"ts-cycle_days-custom\""));
  assert.ok(custom.includes("Use a whole number of days from 1 to 90."));

  const panel = read("src/screens/settings/tickets.tsx");
  assert.match(panel, /const useAppDefault = \(\) => \{\n\s+if \(page\.kind === "ok"\) void send\(\{\}, page\.value\.version\);/);
  assert.match(panel, /result\.kind === "ok" \|\| \(result\.kind === "error" && result\.code === "tracker_settings_stale"\)\) \{\n\s+setDrafts\(\{\}\);\n\s+reload\(\);/);
  const api = read("src/api-ticket-settings.ts");
  assert.match(api, /call<TicketSettingsPage>\("desktop_ticket_settings", \{ projectId \}\)/);
  assert.match(api, /call<void>\("desktop_save_ticket_settings", \{ projectId, values, version \}\)/);
});

test("stale saves, unattached projects and refusals read as plain sentences", () => {
  assert.equal(saveNote({ kind: "ok", value: undefined }), "Saved.");
  assert.equal(saveNote({ kind: "error", code: "tracker_settings_stale", message: "" }), settings.TICKET_SETTINGS_STALE_LINE);
  assert.equal(saveNote({ kind: "error", code: "project_not_attached", message: "" }), settings.NOT_ATTACHED_LINE);
  assert.equal(saveNote({ kind: "not_ready" }), "Not ready yet: changing ticket settings isn't in this build.");
  for (const [code, line] of Object.entries(settings.TICKET_SETTINGS_REFUSALS)) assert.equal(settings.ticketSettingsRefusal({ code }), line, code);
  assert.equal(settings.ticketSettingsRefusal({ code: "disk_on_fire" }), failureCopy({ code: "disk_on_fire" }));
  assert.equal(settings.ticketSettingsRefusal({ code: "toString" }), failureCopy({ code: "toString" }));
  const unattached = view({ kind: "error", code: "project_not_attached", message: "" }, "project");
  assert.ok(unattached.includes(markup(settings.NOT_ATTACHED_LINE)));
  assert.equal(radios(unattached), 0);
  assert.ok(view({ kind: "error", code: "tracker_unavailable", message: "" }, "app").includes(markup("CrazyAgent couldn't reach its tickets. Try again.")));
  assert.equal(radios(view({ kind: "loading" }, "app")), 0);
  assert.equal(roadmap.roadmapRefusal("stale_version"), "This changed since you opened it. The roadmap now shows the saved state; try again.");
  assert.equal(roadmap.roadmapRefusal("import_item_unknown"), "The roadmap file changed since the preview. Preview it again.");
  assert.equal(roadmap.roadmapRefusal("disk_on_fire"), failureCopy("disk_on_fire"));
  assert.throws(() => settings.checkedPage(pageOf("project-b"), "project-a"), /scope it was read for/);
  assert.throws(() => settings.checkedPage({ ...pageOf(null), keys: pageOf(null).keys.slice().reverse() }, null), /every setting once, in order/);
  assert.throws(() => settings.checkedPage({ ...pageOf(null), version: -1 }, null), /has a version/);
  assert.throws(() => view({ kind: "ok", value: pageOf(null) }, "project"), /scope it was read for/);
});

test("roadmap pages join in order and refuse a doubled version, cycle or second active cycle", () => {
  const first = roadmap.roadmapPage({ unplanned: { closed: 0, total: 1 }, versions: [version("1.0", 0, null)], cycles: [cycle(1, "closed", "2026-10-05", "2026-10-18")] });
  const second = roadmap.roadmapPage({ unplanned: { closed: 0, total: 1 }, versions: [version("1.1", 1, "2026-12-01")], cycles: [cycle(2, "active", "2026-10-19", "2026-11-01")] });
  assert.deepEqual(roadmap.joinedRoadmap([first, second]).versions.map((item) => item.name), ["1.0", "1.1"]);
  assert.equal(roadmap.joinedRoadmap([first, first]), null);
  assert.equal(roadmap.joinedRoadmap([second, { ...first, versions: [], cycles: [cycle(3, "active", "2026-11-02", "2026-11-15")] }]), null);
  assert.equal(roadmap.roadmapPage({ unplanned: { closed: 2, total: 1 }, versions: [], cycles: [] }), null);
  assert.equal(roadmap.roadmapPage({ unplanned: { closed: 0, total: 0 }, versions: [version("1.0", 0, "2026-02-30")], cycles: [] }), null);
  assert.equal(roadmap.roadmapPage({ unplanned: { closed: 0, total: 0 }, versions: [], cycles: [cycle(1, "active", "2026-11-15", "2026-11-02")] }), null);
  assert.equal(roadmap.roadmapPage({ unplanned: { closed: 0, total: 0 }, versions: [], cycles: [cycle(1, "running", "2026-11-02", "2026-11-15")] }), null);
  assert.throws(() => roadmap.joinedRoadmap([]), /one to ROADMAP_PAGES_MAX/);
});

test("names and titles from agents or the file show hidden characters", () => {
  const html = roadmapView({ roadmap: board([cycle(3, "active", "2026-11-02", "2026-11-15", { release: "1.0\u202e" })], [version("beta\u200b", 0, null)]) });
  assert.ok(html.includes("for 1.0\\u202e"));
  assert.ok(html.includes("beta\\u200b"));
  assert.ok(!html.includes("\u202e") && !html.includes("\u200b"));
  assert.equal(roadmap.importLine({ change: "ticket_add", version: null, kind: "bug", title: "Fix\u202elogin" }), "New bug: Fix\\u202elogin");
  assert.equal(roadmap.unparsedLine({ heading: "x\u200by", line_first: 4, line_last: 4, why: "second_target", text: "" }), "Line 4 (x\\u200by): the section names a second target");
  const moving = roadmapView({ roadmap: board([cycle(4, "upcoming", "2026-11-16", "2026-11-29")]), closed: { number: 3, rollover: "ask", unfinished: 1, moved_to: null, tickets: ["RB-4\u200b"] } });
  assert.ok(moving.includes("RB-4\\u200b"));
});

test("Settings and the project show the ticket settings and the Roadmap tab", () => {
  assert.equal(SETTINGS_TABS[SETTINGS_TABS.indexOf("process") + 1], "tickets");
  assert.equal(PROJECT_TABS[PROJECT_TABS.indexOf("tickets") + 1], "roadmap");
  const app = read("src/screens/settings/index.tsx");
  assert.match(app, /\{ id: "tickets", label: "Tickets" \}/);
  assert.match(app, /tab === "tickets" \? <TicketSettings \/>/);
  const project = read("src/screens/project/index.tsx");
  assert.match(project, /\{ id: "roadmap", label: "Roadmap", icon: "[a-z-]+" \}/);
  assert.match(project, /active === "roadmap" && <RoadmapTab data=\{data\} \/>/);
  const projectSettings = read("src/screens/project/settings.tsx");
  assert.match(projectSettings, /\{ id: "tickets", label: "Tickets" \}/);
  assert.match(projectSettings, /<TicketSettingsPanel projectId=\{data\.id\} \/>/);
  const tab = read("src/screens/project/roadmap.tsx");
  assert.match(tab, /op\("cycle_close", \{ project: key, number: cycle\.number, version: cycle\.version \}, cycleClosed\)/);
  assert.match(tab, /op\("import_apply", \{ project: key, path: ROADMAP_PATH, accept \}/);
  assert.match(tab, /op\("edit", \{ id: ticket, cycle, version: read\.value\.data \}/);
});
