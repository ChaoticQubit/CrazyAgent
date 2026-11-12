import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-tracker.ts");
const list = await import("../src/screens/project/tickets.tsx");
const detail = await import("../src/screens/project/ticket-detail.tsx");

const NOW = Date.UTC(2026, 10, 3, 9, 0);
const SET = "0123456789abcdef0123456789abcdef";
const row = (extra = {}) => ({ id: "RB-3", title: "Import recipes from a file", status: "todo", priority: 2, assignee: null, ...extra });
const event = (at, op, actor = "owner", fields = ["status"]) => ({ at_ms: at, actor, op, fields });
const ticket = (extra = {}) => ({
  id: "RB-3", version: 4, title: "Import recipes from a file", body_bytes: 11, kind: "feature", status: "todo", priority: 1,
  assignee: "agent:team-2", created_by: "owner", created_ms: NOW - 3_600_000, updated_ms: NOW - 60_000, closed_ms: null,
  closed_by: null, close_reason: null, ...extra,
});
const shown = (extra = {}) => client.mergeShow([{
  ticket: ticket(extra.ticket),
  body_at: 0,
  body: "Read a CSV.",
  criteria: [{
    position: 1, text: "A CSV of 3 recipes imports 3 recipes", state: "open", version: 2,
    checks: [{ kind: "test", ref: "import::three_recipes", ref_elided: false, latest: { result: "pass", evidence: "run 41 passed", by: "engine", at_ms: NOW - 120_000 } }],
  }],
  links: extra.links ?? [{ kind: "change_set", target: SET, live: true, label: "Change set", state: "ready" }],
  events: [event(NOW - 60_000, "move"), event(NOW - 3_600_000, "create", "owner", [])],
}]);
const view = (props = {}) => renderToStaticMarkup(createElement(detail.TicketDetailView, {
  detail: shown(), history: null, now: NOW, busy: false, note: "", onMove: () => {}, onOpenChangeSet: () => {}, onHistory: () => {}, onBack: () => {},
  ...props,
}));

test("api_tracker_op_names_match_schema_v1", () => {
  const schema = JSON.parse(readFileSync(new URL("../../../crates/trackwright-core/schema/v1.json", import.meta.url), "utf8"));
  assert.equal(schema.api, 1);
  const names = Object.keys(schema.ops).sort();
  assert.equal(names.length, 30);
  assert.deepEqual([...client.OP_NAMES].sort(), names, "the desktop names every schema v1 operation and no other");
  assert.deepEqual([...client.WRITE_OPS].sort(), names.filter((name) => schema.ops[name].write), "the writes are the schema's writes");
  for (const name of client.WRITE_OPS) assert.ok(schema.ops[name].args.properties.request_id, `${name} takes a request id`);
  assert.deepEqual([...client.STATUSES], schema.ops.list.args.properties.status.enum);
  assert.deepEqual([...client.KINDS], schema.ops.list.args.properties.kind.enum);
  assert.deepEqual(Object.keys(schema.ops.list.args.properties).sort(), ["assignee", "cursor", "kind", "project", "status"]);
});

test("a write gets one request id of its own and a read gets none", () => {
  const id = "desk-0f6c2a1e-6b1d-4c41-9b0e-6fb7a8c1d2e3";
  assert.deepEqual(client.withRequestId("move", { id: "RB-3", version: 4, status: "in_progress" }, id), { id: "RB-3", version: 4, status: "in_progress", request_id: id });
  assert.deepEqual(client.withRequestId("show", { id: "RB-3" }, id), { id: "RB-3" });
  assert.deepEqual(client.withRequestId("move", { id: "RB-3", request_id: "kept" }, id), { id: "RB-3", request_id: "kept" }, "a caller's own id is kept for a retry");
  const fresh = client.requestId();
  assert.match(fresh, /^[A-Za-z0-9._:-]{1,64}$/);
  assert.notEqual(fresh, client.requestId());
  const source = readFileSync(new URL("../src/api-tracker.ts", import.meta.url), "utf8");
  assert.match(source, /call<TrackerEnvelope>\("desktop_tracker_op", \{ op, args: withRequestId\(op, args, requestId\(\)\) \}\)/, "the window sends only the operation and its input");
  assert.match(source, /call<TrackedProject\[\]>\("desktop_tracker_projects", \{\}\)/);
  assert.match(source, /call<TrackedProject>\("desktop_tracker_attach", \{ projectId, key, name \}\)/);
  assert.match(source, /call<void>\("desktop_tracker_open_change_set", \{ projectId, changeSet \}\)/);
});

test("list filters send only what the owner chose", () => {
  const none = { status: "", kind: "", assignee: "" };
  assert.deepEqual(client.listArgs("RB", none, null), { project: "RB" });
  assert.deepEqual(client.listArgs("RB", { status: "todo", kind: "", assignee: "" }, null), { project: "RB", status: "todo" });
  assert.deepEqual(client.listArgs("RB", { status: "", kind: "bug", assignee: "  agent:team-2 " }, "tickets:50"), { project: "RB", kind: "bug", assignee: "agent:team-2", cursor: "tickets:50" });
  assert.deepEqual(client.listArgs("RB", { status: "triage", kind: "spike", assignee: " " }, null), { project: "RB", status: "triage", kind: "spike" });
  assert.equal(client.filtersActive(none), false);
  assert.equal(client.filtersActive({ ...none, kind: "chore" }), true);
});

test("the board groups tickets by status, most urgent first, and closed ones in Closed", () => {
  const columns = list.boardColumns([
    row({ id: "RB-9", status: "todo", priority: 3 }),
    row({ id: "RB-2", status: "todo", priority: 3 }),
    row({ id: "RB-12", status: "todo", priority: 0 }),
    row({ id: "RB-4", status: "triage" }),
    row({ id: "RB-5", status: "done" }),
    row({ id: "RB-6", status: "cancelled", priority: 0 }),
    row({ id: "RB-7", status: "in_review" }),
  ]);
  assert.deepEqual(columns.map((column) => column.status), ["triage", "backlog", "todo", "in_progress", "in_review", "closed"]);
  assert.deepEqual(columns.map((column) => column.label), ["Triage", "Backlog", "To do", "In progress", "In review", "Closed"]);
  const ids = Object.fromEntries(columns.map((column) => [column.status, column.rows.map((item) => item.id)]));
  assert.deepEqual(ids, { triage: ["RB-4"], backlog: [], todo: ["RB-12", "RB-2", "RB-9"], in_progress: [], in_review: ["RB-7"], closed: ["RB-6", "RB-5"] });
  assert.deepEqual(columns.map((column) => column.drop), [true, true, true, true, true, false], "closing takes evidence, so Closed takes no drop");
  const todo = columns[2];
  assert.equal(list.canDrop(row({ status: "backlog" }), todo), true);
  assert.equal(list.canDrop(row({ status: "todo" }), todo), false, "dropping where it is changes nothing");
  assert.equal(list.canDrop(row({ status: "done" }), todo), false, "a closed ticket is reopened, not dragged");
  assert.equal(list.canDrop(row({ status: "todo" }), columns[5]), false);
});

test("a board move is a versioned write from a fresh read, and a ticket that moved meanwhile is stale", () => {
  const seen = row({ status: "backlog" });
  assert.deepEqual(list.boardMove(seen, ticket({ status: "backlog", version: 7 }), "todo"), { kind: "move", id: "RB-3", version: 7, status: "todo" });
  assert.deepEqual(list.boardMove(seen, ticket({ status: "in_progress", version: 8 }), "todo"), { kind: "stale" });
  assert.equal(client.STALE_LINE, "This ticket changed. Review it again.");
  assert.equal(client.trackerRefusal({ code: "stale_version" }), client.STALE_LINE);
  const refused = client.opened({ kind: "ok", value: { api: 1, ok: false, code: "stale_version", message: "version 4 is stale; the ticket is at 5", next: [{ op: "show", id: "RB-3", why: "read it again" }] } });
  assert.deepEqual(refused, { kind: "error", code: "stale_version", message: client.STALE_LINE, cause: "version 4 is stale; the ticket is at 5" });
  const fine = client.opened({ kind: "ok", value: { api: 1, ok: true, data: { tickets: [] }, next: [], cursor: "tickets:50" } });
  assert.deepEqual(fine, { kind: "ok", value: { data: { tickets: [] }, cursor: "tickets:50" } });
  assert.deepEqual(client.opened({ kind: "not_ready" }), { kind: "not_ready" });
  const markup = view({ note: client.STALE_LINE });
  assert.match(markup, /<p class="pj-note warn" role="alert">This ticket changed\. Review it again\.<\/p>/);
});

test("every refusal reads as a sentence and an unknown code falls back to plain copy", () => {
  assert.ok(client.TRACKER_REFUSALS.includes("stale_version"));
  assert.ok(client.TRACKER_REFUSALS.includes("ide_not_running"));
  for (const code of client.TRACKER_REFUSALS) {
    const line = client.trackerRefusal({ code });
    assert.match(line, /^[A-Z].*[.]$/, `${code}: ${line}`);
    assert.doesNotMatch(line, /[_\u2014]/, `${code} reads as words`);
  }
  assert.match(client.trackerRefusal({ code: "tracker_new_code" }), /tracker_new_code/);
  assert.equal(client.notReady({ kind: "not_ready" }), true);
  assert.equal(client.notReady({ kind: "error", code: "tracker_unavailable", message: "" }), true);
  assert.equal(client.notReady({ kind: "error", code: "stale_version", message: "" }), false);
});

test("a gone link says Gone and offers no Open in IDE; a live change set opens in the IDE", () => {
  assert.deepEqual(client.linkView({ kind: "change_set", target: SET, live: true, label: "Change set", state: "ready" }),
    { kind: "Change set", title: `Change set ${SET}`, state: "ready", live: true, changeSet: SET });
  assert.deepEqual(client.linkView({ kind: "change_set", target: SET, live: false, label: "Change set", state: "gone" }),
    { kind: "Change set", title: `Change set ${SET}`, state: "Gone", live: false, changeSet: null });
  assert.deepEqual(client.linkView({ kind: "agent", target: "7", live: false, label: "", state: "link_broken" }),
    { kind: "Agent", title: "Agent 7", state: "Gone", live: false, changeSet: null });
  assert.deepEqual(client.linkView({ kind: "agent", target: "7", live: true, label: "Agent 7 (sub)", state: "running" }),
    { kind: "Agent", title: "Agent 7 (sub)", state: "running", live: true, changeSet: null });
  assert.equal(client.linkView({ kind: "change_set", target: "not-an-id", live: true, label: "Change set", state: "ready" }).changeSet, null);
  assert.equal(client.linkView({ kind: "wiki", target: "Home", live: true, label: "", state: "unknown" }).kind, "wiki");
  const live = view();
  assert.match(live, /<button[^>]*>.*Open in IDE<\/button>/);
  const gone = view({ detail: shown({ links: [{ kind: "change_set", target: SET, live: false, label: "Change set", state: "gone" }] }) });
  assert.match(gone, />Gone</);
  assert.doesNotMatch(gone, /Open in IDE/);
});

test("history reads newest first: the detail's recent events as sent, the full history reversed", () => {
  const recent = [event(30, "move"), event(20, "edit", "agent:team-2", ["title"]), event(10, "create", "owner", [])];
  assert.deepEqual(client.newestFirst(recent, null).map((item) => item.at_ms), [30, 20, 10]);
  const pages = [[event(10, "create", "owner", []), event(20, "edit")], [event(30, "move"), event(40, "link", "agent:team-2", ["links"])]];
  assert.deepEqual(client.newestFirst(recent, pages).map((item) => item.at_ms), [40, 30, 20, 10]);
  assert.deepEqual(client.newestFirst([], []), []);
  const markup = view();
  const moved = markup.indexOf("moved");
  const created = markup.indexOf("created");
  assert.ok(moved > 0 && created > moved, "the newest event is on top");
  assert.match(markup, />Show full history</);
  const full = view({ history: pages });
  assert.doesNotMatch(full, />Show full history</);
  assert.ok(full.indexOf("linked") < full.indexOf("created"));
});

test("show pages merge into one ticket: body in order, criteria, links and recent events", () => {
  const merged = client.mergeShow([
    { ticket: ticket(), body_at: 0, body: "Read a ", criteria: [], links: [], events: [] },
    { ticket: { id: "RB-3", version: 4, body_bytes: 11 }, body_at: 7, body: "CSV.", criteria: [{ position: 1, text: "Imports", state: "met", version: 1, checks: [] }] },
    { ticket: { id: "RB-3", version: 4, body_bytes: 11 }, links: [{ kind: "task", target: "t-1", live: true, label: "Task t-1", state: "done" }], events: [event(5, "create")] },
  ]);
  assert.equal(merged.ticket.title, "Import recipes from a file", "the first page carries the fields");
  assert.equal(merged.body, "Read a CSV.");
  assert.deepEqual(merged.criteria.map((item) => item.text), ["Imports"]);
  assert.deepEqual(merged.links.map((item) => item.target), ["t-1"]);
  assert.deepEqual(merged.events.map((item) => item.at_ms), [5]);
});

test("agent text shows through displayText and Triage shows a badge", async () => {
  const { displayText } = await import("../src/screens/conversation.tsx");
  const hostile = "Fix login\u202egnp.exe";
  assert.notEqual(displayText(hostile), hostile);
  const rows = renderToStaticMarkup(createElement(list.TicketList, { rows: [row({ title: hostile, status: "triage", assignee: "agent:\u200bteam-2" })], onOpen: () => {} }));
  assert.ok(rows.includes(displayText(hostile)));
  assert.ok(!rows.includes(hostile));
  assert.ok(rows.includes(displayText("agent:\u200bteam-2")));
  assert.match(rows, />Triage</, "a triage ticket carries its badge");
  const plain = renderToStaticMarkup(createElement(list.TicketList, { rows: [row()], onOpen: () => {} }));
  assert.doesNotMatch(plain, />Triage</);
  const board = renderToStaticMarkup(createElement(list.TicketBoard, { columns: list.boardColumns([row({ title: hostile, status: "triage" })]), busy: false, onMove: () => {}, onOpen: () => {} }));
  assert.ok(board.includes(displayText(hostile)) && !board.includes(hostile));
  assert.match(board, /draggable="true"/);
  const ready = renderToStaticMarkup(createElement(list.ReadyList, { rows: [{ ...row({ title: hostile }), cycle: 3, release: "v\u200b1" }], onOpen: () => {} }));
  assert.ok(ready.includes(displayText(hostile)) && ready.includes(displayText("v\u200b1")));
  const markup = view({
    detail: shown({
      ticket: { title: hostile, status: "triage" },
      links: [{ kind: "branch", target: "feat/x", live: true, label: "feat/x\u202e (agent 2)", state: "checked\u200bout" }],
    }),
  });
  assert.ok(markup.includes(displayText(hostile)) && !markup.includes(hostile));
  assert.ok(markup.includes(displayText("feat/x\u202e (agent 2)")));
  assert.ok(markup.includes(displayText("checked\u200bout")));
  assert.match(markup, />Triage</);
  assert.match(markup, />A CSV of 3 recipes imports 3 recipes</);
  assert.match(markup, />import::three_recipes</);
  assert.match(markup, />Passed: run 41 passed</);
});

test("a project without tickets offers a key from its name, and keys are 2 to 8 capital letters", () => {
  assert.equal(client.suggestKey("Recipe box"), "RB");
  assert.equal(client.suggestKey("crazy agent desktop app"), "CADA");
  assert.equal(client.suggestKey("Notes"), "NOTES");
  assert.equal(client.suggestKey("x"), "XX");
  assert.equal(client.suggestKey("  "), "PROJ");
  assert.equal(client.suggestKey("Über café 9"), "BC", "letters outside A to Z are dropped");
  assert.equal(client.suggestKey("a b c d e f g h i j"), "ABCDEFGH");
  assert.equal(client.suggestKey("Supercalifragilistic"), "SUPERCAL");
  assert.equal(client.keyProblem("RB"), null);
  assert.equal(client.keyProblem("ABCDEFGH"), null);
  for (const key of ["", "R", "rb", "RB1", "ABCDEFGHI", "R B"]) assert.match(client.keyProblem(key), /2 to 8 capital letters/, key);
  const projects = [{ project: "p-1", key: "RB", name: "Recipe box" }, { project: "p-2", key: "CA", name: "CrazyAgent" }];
  assert.deepEqual(client.trackedProject(projects, "p-2"), projects[1]);
  assert.equal(client.trackedProject(projects, "p-9"), null);
});

test("the Tickets tab sits after Questions and both tracker tabs route", async () => {
  const { TABS } = await import("../src/screens/project/index.tsx");
  assert.deepEqual(TABS.map((tab) => tab.id), ["overview", "conversation", "agents", "changes", "tests", "questions", "tickets", "roadmap", "context", "process", "metrics", "settings"]);
  assert.equal(TABS.find((tab) => tab.id === "tickets").label, "Tickets");
  const router = await import("../src/ui/router.ts");
  for (const tab of ["tickets", "roadmap"]) {
    assert.ok(router.PROJECT_TABS.includes(tab), tab);
    assert.deepEqual(router.parseRoute(router.formatRoute({ name: "project", id: "p-1", tab })), { name: "project", id: "p-1", tab });
  }
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /\{active === "tickets" && <TicketsTab data=\{data\} \/>\}/);
});

test("a project's ticket settings use the Settings section parts and offer Use app default once it sets a key", async () => {
  const settings = await import("../src/screens/settings/tickets.tsx");
  const keys = [
    ["close", ["agents", "evidence", "owner"], "evidence", "owner"],
    ["create", ["direct", "triage", "owner_only"], "triage", null],
    ["agent_edit", ["any", "status_and_criteria", "none"], "status_and_criteria", null],
    ["cycle_days", [7, 14, 21, 28], 14, null],
    ["rollover", ["auto", "ask", "off"], "auto", null],
    ["import", ["auto_once", "on_request"], "auto_once", null],
    ["manual_evidence", ["owner", "agents"], "owner", null],
    ["ci_evidence", ["engine", "owner", "agents"], "engine", null],
  ].map(([key, options, below, here]) => ({ key, options, proposed: below, custom: key === "cycle_days" ? { min: 1, max: 90 } : null, here, below, effective: here ?? below }));
  const html = renderToStaticMarkup(createElement(settings.TicketSettingsView, {
    page: { kind: "ok", value: { project_id: "alpha", version: 2, keys } }, scope: "project", projectId: "alpha", drafts: {}, busy: false, note: "",
    onDraft: () => {}, onSave: () => {}, onUseAppDefault: () => {}, onRetry: () => {},
  }));
  assert.match(html, /class="pj set-groups"/);
  assert.match(html, /class="set-group"/);
  assert.match(html, /Ticket rules/);
  assert.match(html, /A value set here wins over the app value for this project only\./);
  assert.match(html, /App default \(When the evidence is in\)/);
  assert.equal((html.match(/class="set-row wide"/g) ?? []).length, 8);
  assert.equal((html.match(/class="set-changed"/g) ?? []).length, 1, "only Closing tickets differs from the app value");
  assert.match(html, /Use app default/);
  assert.match(html, /Save changes/);
});
