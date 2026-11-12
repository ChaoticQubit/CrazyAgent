import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-notifications.ts");
const panel = await import("../src/screens/notifications/panel.tsx");
const settings = await import("../src/screens/settings/notifications.tsx");
const { SETTINGS_GROUPS } = await import("../src/screens/settings/index.tsx");
const { SETTINGS_TABS } = await import("../src/ui/router.ts");

const ENGINE = readFileSync(new URL("../../../crates/crazyagent/src/notifications.rs", import.meta.url), "utf8");
const DASHBOARD = readFileSync(new URL("../src/screens/dashboard/index.tsx", import.meta.url), "utf8");

const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/\s+/gu, " ").trim();
const question = (id, extra = {}) => ({
  id, kind: "question", class: "action", project_id: "p-1", text: "Master asks: Ship it?",
  target: { page: "questions", project_id: "p-1", question: 7, agent: null },
  created_ms: 1_800_000_000_000, read_ms: null, resolved_ms: null, folded: 1, unread: 1, pending: 1, ids: [id], banner: true, ...extra,
});
const finished = (id, extra = {}) => ({
  id, kind: "agent_finished", class: "info", project_id: "p-1", text: "Sub-agent 2 finished Add the login form",
  target: { page: "agents", project_id: "p-1" },
  created_ms: 1_800_000_000_000, read_ms: null, resolved_ms: null, folded: 1, unread: 1, pending: 0, ids: [id], banner: true, ...extra,
});
const page = (items, counts = { unread: 2, pending: 1 }) => ({ kind: "ok", value: { items, counts, more: false, newest: items[0]?.id ?? 0 } });
const projects = [{ id: "p-1", name: "Recipe box" }, { id: "p-2", name: "Tool shop" }];
const panelView = (loaded, extra = {}) => renderToStaticMarkup(createElement(panel.NoticePanelView, {
  page: loaded, filter: "all", projectId: null, projects, selected: new Set(), problem: "",
  onFilter: forbidden, onProject: forbidden, onOpen: forbidden, onSelect: forbidden, onMarkAllRead: forbidden,
  onReadSelected: forbidden, onUnreadSelected: forbidden, onDeleteSelected: forbidden, onRetry: forbidden, ...extra,
}));
const settingsOk = (value = api.NOTICE_DEFAULTS) => ({ kind: "ok", value });
const settingsView = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.NotificationSettingsView, {
  settings: loaded, kinds: null, banners: null, typedDays: null, saving: false, problem: "",
  onPickDelivery: forbidden, onPickBanners: forbidden, onTypeDays: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const checked = (markup, name) => [...markup.matchAll(/<input[^>]*type="radio"[^>]*>/g)]
  .map(([input]) => input)
  .filter((input) => input.includes(`name="${name}"`) && / checked=""/.test(input))
  .map((input) => input.match(/ value="([a-z_]+)"/)[1]);
const values = (markup, name) => [...markup.matchAll(/<input[^>]*type="radio"[^>]*>/g)]
  .map(([input]) => input)
  .filter((input) => input.includes(`name="${name}"`))
  .map((input) => input.match(/ value="([a-z_]+)"/)[1]);
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);

test("every kind has one engine name, its class and the label the engine gives it", () => {
  const body = (start, end) => ENGINE.slice(ENGINE.indexOf(start), ENGINE.indexOf(end));
  const arms = (text) => new Map([...text.matchAll(/Self::(\w+) => "([^"]+)",/g)].map(([, variant, words]) => [variant, words]));
  const names = arms(body("pub fn name(self)", "pub fn label(self)"));
  const labels = arms(body("pub fn label(self)", "pub fn parse("));
  assert.equal(api.NOTICE_KINDS.length, 20);
  assert.equal(labels.size, 20);
  for (const entry of api.NOTICE_KINDS) {
    const variant = entry.kind.split("_").map((part) => part[0].toUpperCase() + part.slice(1)).join("");
    assert.equal(names.get(variant), entry.kind, `the engine names ${entry.kind}`);
    assert.equal(labels.get(variant), entry.label, `the engine labels ${entry.kind}`);
  }
  assert.deepEqual(api.NOTICE_KINDS.filter((entry) => entry.class === "action").map((entry) => entry.kind),
    ["question", "account_sign_in", "research_pool", "memory_review", "change_set_review"]);
  assert.equal(api.kindLabel("change_set_landed"), "Change set landed");
});

test("each target opens its exact page, with the anchor of the item to highlight", () => {
  const opened = (target) => { const { hash, anchor } = api.routeOf(target); return [hash, anchor]; };
  assert.deepEqual(opened({ page: "questions", project_id: "p-1", question: 7, agent: null }), ["#/projects/p-1/questions", "question-7"]);
  assert.deepEqual(opened({ page: "questions", project_id: "p-1", question: null, agent: 3 }), ["#/projects/p-1/agents?section=questions", "agent-3"], "an agent's question or wait is in the Agents tab's drawer, not the owner Questions tab");
  assert.deepEqual(opened({ page: "account_sign_in", account: "work-claude" }), ["#/settings/accounts?action=sign_in&account=work-claude", null]);
  assert.deepEqual(opened({ page: "project_pool", project_id: "p-1" }), ["#/projects/p-1/settings?section=pool", null]);
  assert.deepEqual(opened({ page: "memories", project_id: "p-1" }), ["#/projects/p-1/context?file=%40memory", null]);
  assert.deepEqual(opened({ page: "change_set", project_id: "p-1", change_set: "a".repeat(32) }), ["#/projects/p-1/changes", `change-set-${"a".repeat(32)}`]);
  assert.deepEqual(opened({ page: "change_set", project_id: "p-1", change_set: null }), ["#/projects/p-1/changes", null]);
  assert.deepEqual(opened({ page: "agents", project_id: "p-1" }), ["#/projects/p-1/agents", null]);
  assert.deepEqual(opened({ page: "conversation", project_id: "p-1" }), ["#/projects/p-1/conversation", null]);
  assert.deepEqual(opened({ page: "accounts" }), ["#/settings/accounts", null]);
  assert.deepEqual(opened({ page: "usage" }), ["#/metrics", null]);
  assert.deepEqual(opened({ page: "updates" }), ["#/settings/updates", null]);
  assert.deepEqual(opened({ page: "harnesses" }), ["#/settings/harnesses", null]);
  assert.deepEqual(opened({ page: "home" }), ["#/home", null]);
});

test("a badge shows its count up to 99, then 99+, and nothing at zero", () => {
  assert.equal(api.badgeText(0), null);
  assert.equal(api.badgeText(1), "1");
  assert.equal(api.badgeText(99), "99");
  assert.equal(api.badgeText(100), "99+");
  assert.equal(api.badgeText(9999), "99+");
});

test("the bell shows the unread and pending badges and names both for a screen reader", () => {
  const markup = renderToStaticMarkup(createElement(panel.NoticeBellView, { counts: { unread: 3, pending: 1 }, open: false, onToggle: forbidden }));
  assert.match(markup, /aria-label="Notifications: 3 unread, 1 pending"/);
  assert.match(markup, /aria-expanded="false"/);
  assert.match(markup, /class="notice-badge unread"[^>]*>3</);
  assert.match(markup, /class="notice-badge pending"[^>]*>1</);
  const quiet = renderToStaticMarkup(createElement(panel.NoticeBellView, { counts: { unread: 0, pending: 0 }, open: true, onToggle: forbidden }));
  assert.match(quiet, /aria-label="Notifications: nothing new"/);
  assert.match(quiet, /aria-expanded="true"/);
  assert.doesNotMatch(quiet, /notice-badge/);
});

test("the panel has the four filters, the project picker, Mark all read and one row per item with its project and page", () => {
  const markup = panelView(page([question(12), finished(11)]));
  const shown = text(markup);
  assert.deepEqual(values(markup, "notice-filter"), ["all", "unread", "pending", "done"]);
  assert.deepEqual(checked(markup, "notice-filter"), ["all"]);
  assert.match(shown, /All Unread Pending Done/);
  assert.match(markup, /<select[^>]*aria-label="Project"/);
  assert.match(shown, /All projects Recipe box Tool shop/);
  assert.match(shown, /Mark all read/);
  assert.match(shown, /Question Recipe box Master asks: Ship it\?/);
  assert.match(shown, /Agent finished Recipe box Sub-agent 2 finished Add the login form/);
  assert.match(markup, /data-notice="12"[^>]*class="notice-row unread pending"/);
  assert.match(markup, /data-notice="11"[^>]*class="notice-row unread"/);
});

test("same-kind items of a project fold into one row that says how many it holds", () => {
  const folded = finished(30, { folded: 4, unread: 2, ids: [30, 29, 28, 27], text: "Sub-agent 5 finished Fix the cart" });
  const shown = text(panelView(page([folded])));
  assert.match(shown, /Agent finished Recipe box Sub-agent 5 finished Fix the cart 4 like this, 2 unread/);
});

test("each empty filter says so in its own words", () => {
  const empty = (filter) => text(panelView(page([], { unread: 0, pending: 0 }), { filter }));
  assert.match(empty("all"), /No notifications yet\./);
  assert.match(empty("unread"), /You're all caught up\./);
  assert.match(empty("pending"), /Nothing waits for you\./);
  assert.match(empty("done"), /Nothing is done yet\./);
});

test("selected rows offer Mark read, Mark unread and Delete for how many are picked", () => {
  const shown = text(panelView(page([question(12), finished(11)]), { selected: new Set([12, 11]) }));
  assert.match(shown, /2 selected Mark read Mark unread Delete/);
  const none = text(panelView(page([question(12)])));
  assert.doesNotMatch(none, /selected/);
});

test("a failed read offers Try again", () => {
  const shown = text(panelView({ kind: "error", code: "owner_read_failed", message: "Couldn't reach the engine." }));
  assert.match(shown, /Couldn't load notifications\. Couldn't reach the engine\. Try again/);
});

test("a change to panel rows reaches every item each row folds, scoped by the filter it was listed under", () => {
  const folded = finished(30, { folded: 40, ids: [30, 29] });
  const sign = { ...question(12), kind: "account_sign_in", project_id: null, target: { page: "account_sign_in", account: "work" } };
  assert.deepEqual(api.foldsAction([folded, sign], "unread", "read"), { folds: { change: "read", folds: [
    { kind: "agent_finished", project_id: "p-1", filter: "unread", newest: 30 },
    { kind: "account_sign_in", project_id: null, filter: "unread", newest: 12 },
  ] } });
  assert.deepEqual(api.foldsAction([folded], "all", "delete").folds.change, "delete");
  assert.equal(api.foldsAction([], "all", "read"), null);
  assert.equal(api.foldsAction(Array.from({ length: api.PAGE_MAX + 1 }, (_, index) => finished(index + 1)), "all", "read"), null);
});

test("opening a row goes to its exact page, marks the row read and leaves its anchor to highlight", async () => {
  const went = [];
  const sent = [];
  const send = async (action) => { sent.push(action); return { kind: "ok", value: undefined }; };
  const go = (hash) => went.push(hash);
  const folded = finished(30, { folded: 2, ids: [30, 29] });
  assert.deepEqual(await panel.openNotice(folded, "all", { go, send }), { anchor: null, problem: "" });
  assert.deepEqual(went, ["#/projects/p-1/agents"]);
  assert.deepEqual(sent, [{ folds: { change: "read", folds: [{ kind: "agent_finished", project_id: "p-1", filter: "all", newest: 30 }] } }]);
  const read = question(12, { read_ms: 5, unread: 0 });
  assert.deepEqual(await panel.openNotice(read, "pending", { go, send }), { anchor: "question-7", problem: "" });
  assert.equal(sent.length, 1, "a row already read sends nothing");
  assert.deepEqual(went, ["#/projects/p-1/agents", "#/projects/p-1/questions"]);
});

test("opening one unfolded item reads only that item, so other accounts' sign-ins stay unread", async () => {
  const sent = [];
  const send = async (action) => { sent.push(action); return { kind: "ok", value: undefined }; };
  const sign = { ...question(21), kind: "account_sign_in", project_id: null, text: "home isn't signed in.", target: { page: "account_sign_in", account: "home" } };
  assert.deepEqual(await panel.openNotice(sign, "pending", { go: () => {}, send }), { anchor: null, problem: "" });
  assert.deepEqual(sent, [{ read: { ids: [21] } }], "never a fold read, which reaches every older sign-in");
});

test("a refused read on open says why and still opens the page", async () => {
  const went = [];
  const refused = async () => ({ kind: "error", code: "pending_intent", message: "Another change is still being saved." });
  const opened = await panel.openNotice(finished(30), "all", { go: (hash) => went.push(hash), send: refused });
  assert.deepEqual(opened, { anchor: null, problem: "Another change is still being saved." });
  assert.deepEqual(went, ["#/projects/p-1/agents"]);
});

test("a change to the store says nothing once it stands and the plain refusal when it does not", async () => {
  const ok = async () => ({ kind: "ok", value: undefined });
  const refused = async () => ({ kind: "error", code: "notification_folds_invalid", message: "Those items changed. Try again." });
  const notReady = async () => ({ kind: "not_ready" });
  assert.equal(await panel.changeProblem({ read_all: { project_id: null } }, ok), "");
  assert.equal(await panel.changeProblem({ read_all: { project_id: null } }, refused), "Those items changed. Try again.");
  assert.match(await panel.changeProblem({ read_all: { project_id: null } }, notReady), /not ready/);
});

test("a refused Delete keeps the picks and shows the refusal in the panel", () => {
  const markup = panelView(page([question(12), finished(11)]), { selected: new Set([12]), problem: "Another change is still being saved." });
  assert.match(markup, /role="alert"[^>]*>Another change is still being saved\.</);
  assert.match(text(markup), /1 selected Mark read Mark unread Delete/);
  assert.doesNotMatch(panelView(page([question(12)])), /role="alert"/, "no problem, no alert");
});

test("picks of rows no longer listed are dropped when the page reloads", () => {
  assert.deepEqual([...panel.listedPicks(new Set([12, 99]), page([question(12), finished(11)]).value)], [12]);
  assert.deepEqual([...panel.listedPicks(new Set([99]), page([]).value)], []);
});

test("project names and item text show direction and hidden characters as escapes in the panel", () => {
  const shown = panelView(page([finished(11, { text: "Sub-agent 2 finished fix\u202eexe" })]), { projects: [{ id: "p-1", name: "invoice\u202efdp.exe" }] });
  assert.ok(!shown.includes("\u202e"), shown);
  assert.match(shown, /invoice\\u202efdp\.exe/);
});

test("the project Needs you view lists only that project's pending items, folded", () => {
  assert.deepEqual(api.projectNeedsQuery("p-1"), { filter: "pending", project_id: "p-1", kind: null, fold: true, before: null, after: null, limit: api.PAGE_MAX });
  const markup = renderToStaticMarkup(createElement(panel.ProjectNeedsYouView, { page: page([question(12)]), onOpen: forbidden, onRetry: forbidden }));
  const shown = text(markup);
  assert.match(shown, /Needs you/);
  assert.match(shown, /Question Master asks: Ship it\?/);
  const empty = renderToStaticMarkup(createElement(panel.ProjectNeedsYouView, { page: page([], { unread: 0, pending: 0 }), onOpen: forbidden, onRetry: forbidden }));
  assert.equal(empty, "", "nothing waiting shows nothing");
});

test("the project Needs you view adds the sign-ins of the accounts in the project's pool, and only those", () => {
  const sign = (id, account) => ({ ...question(id), kind: "account_sign_in", project_id: null, text: `${account} isn't signed in.`, target: { page: "account_sign_in", account } });
  assert.deepEqual(api.signInsQuery(), { filter: "pending", project_id: null, kind: "account_sign_in", fold: false, before: null, after: null, limit: api.PAGE_MAX });
  const own = page([question(12)]);
  const signIns = page([sign(20, "work"), sign(21, "home")]);
  const merged = api.projectNeeds(own, signIns, ["work"]);
  assert.equal(merged.kind, "ok");
  assert.deepEqual(merged.value.items.map((item) => item.id), [20, 12]);
  assert.deepEqual(api.projectNeeds(own, signIns, []).value.items.map((item) => item.id), [12]);
  assert.deepEqual(api.projectNeeds(own, { kind: "error", code: "x", message: "y" }, ["work"]).value.items.map((item) => item.id), [12], "a failed sign-in read leaves the project's own items");
  assert.equal(api.projectNeeds({ kind: "loading" }, signIns, ["work"]).kind, "loading");
});

test("the Overview mounts the project Needs you view above its tabs", () => {
  const overview = readFileSync(new URL("../src/screens/project/overview.tsx", import.meta.url), "utf8");
  assert.ok(overview.indexOf("<ProjectNeedsYou") > 0, "the Overview mounts the view");
  assert.ok(overview.indexOf("<ProjectNeedsYou") < overview.indexOf("<ProjectOverviewTabs"), "the view comes before the tab strip");
  assert.doesNotMatch(overview, /id="pj-needs"/, "the old computed lines are gone");
});

test("the dashboard has no Needs you strip: the bell holds what needs the owner", () => {
  assert.doesNotMatch(DASHBOARD, /aria-label="Needs you"/);
  assert.doesNotMatch(DASHBOARD, /export function needsYou/);
});

test("Action kinds can be On or Bell only, never Off; Info kinds can also be Off", () => {
  assert.deepEqual(api.deliveryChoices("question"), ["on", "bell_only"]);
  assert.deepEqual(api.deliveryChoices("change_set_review"), ["on", "bell_only"]);
  assert.deepEqual(api.deliveryChoices("agent_finished"), ["on", "bell_only", "off"]);
  assert.deepEqual(api.deliveryChoices("team_alert"), ["on", "bell_only", "off"]);
});

test("the Done retention takes whole days from 1 to 365", () => {
  assert.equal(api.retentionDays("1"), 1);
  assert.equal(api.retentionDays(" 30 "), 30);
  assert.equal(api.retentionDays("365"), 365);
  for (const typed of ["0", "366", "1.5", "", "abc", "-3", "1e2"]) assert.equal(api.retentionDays(typed), null, typed);
});

test("before the first save every kind is On, Done items stay 30 days and macOS shows all of them", () => {
  assert.deepEqual(api.NOTICE_DEFAULTS.kinds.map((item) => item.kind), api.NOTICE_KINDS.map((entry) => entry.kind));
  assert.ok(api.NOTICE_DEFAULTS.kinds.every((item) => item.delivery === "on"));
  assert.equal(api.NOTICE_DEFAULTS.retention_days, 30);
  assert.equal(api.NOTICE_DEFAULTS.banners, "all");
  assert.deepEqual([api.RETENTION_DAYS_MIN, api.RETENTION_DAYS_DEFAULT, api.RETENTION_DAYS_MAX], [1, 30, 365]);
});

test("Settings lists Notifications in the App group", () => {
  assert.ok(SETTINGS_TABS.includes("notifications"));
  const app = SETTINGS_GROUPS.find((group) => group.title === "App");
  assert.ok(app.items.some((item) => item.id === "notifications" && item.label === "Notifications"));
});

test("Settings, Notifications lists every kind with its stored choice, Off only for Info kinds, the days and the three macOS choices", () => {
  const stored = { ...api.NOTICE_DEFAULTS, kinds: api.NOTICE_DEFAULTS.kinds.map((item) => item.kind === "agent_finished" ? { ...item, delivery: "off" } : item.kind === "question" ? { ...item, delivery: "bell_only" } : item) };
  const markup = settingsView(settingsOk(stored));
  const shown = text(markup);
  assert.match(shown, /What reaches you, and how long finished items stay\./);
  for (const entry of api.NOTICE_KINDS) assert.match(shown, new RegExp(entry.label));
  assert.deepEqual(values(markup, "delivery-question"), ["on", "bell_only"]);
  assert.deepEqual(checked(markup, "delivery-question"), ["bell_only"]);
  assert.deepEqual(values(markup, "delivery-agent_finished"), ["on", "bell_only", "off"]);
  assert.deepEqual(checked(markup, "delivery-agent_finished"), ["off"]);
  assert.deepEqual(values(markup, "banners"), ["off", "action_only", "all"]);
  assert.deepEqual(checked(markup, "banners"), ["all"]);
  assert.match(shown, /On Bell only Off/);
  assert.match(shown, /Off Action items only All/);
  assert.match(markup, /id="notice-retention-days"[^>]*value="30"/);
  assert.ok(saveDisabled(markup), "Save waits for a change");
});

test("a picked choice or typed days enables Save; days out of bounds hold it and say why", () => {
  assert.ok(!saveDisabled(settingsView(settingsOk(), { kinds: new Map([["team_alert", "off"]]) })));
  assert.ok(!saveDisabled(settingsView(settingsOk(), { banners: "action_only" })));
  assert.ok(!saveDisabled(settingsView(settingsOk(), { typedDays: "7" })));
  const refused = settingsView(settingsOk(), { typedDays: "400" });
  assert.ok(saveDisabled(refused));
  assert.match(text(refused), /Enter a whole number of days from 1 to 365\./);
});

test("the settings a save sends keep every kind in engine order with the picks applied", () => {
  const sent = settings.savedSettings(api.NOTICE_DEFAULTS, new Map([["team_alert", "off"], ["question", "bell_only"]]), "action_only", "7");
  assert.equal(sent.kinds.length, 20);
  assert.deepEqual(sent.kinds.map((item) => item.kind), api.NOTICE_KINDS.map((entry) => entry.kind));
  assert.equal(sent.kinds.find((item) => item.kind === "team_alert").delivery, "off");
  assert.equal(sent.kinds.find((item) => item.kind === "question").delivery, "bell_only");
  assert.equal(sent.banners, "action_only");
  assert.equal(sent.retention_days, 7);
  assert.equal(settings.savedSettings(api.NOTICE_DEFAULTS, new Map(), null, "0"), null);
});

test("a not-ready engine and a failed read each say so under the title", () => {
  assert.match(text(settingsView({ kind: "not_ready" })), /Notifications .*not ready/i);
  assert.match(text(settingsView({ kind: "error", code: "owner_read_failed", message: "Couldn't reach the engine." })), /Couldn't load the notification settings\. Couldn't reach the engine\./);
});
