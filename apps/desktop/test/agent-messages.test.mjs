import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const api = await import("../src/api-messages.ts");
const messages = await import("../src/screens/team/agent-messages.tsx");
const team = await import("../src/screens/project/team.tsx");
const settings = await import("../src/screens/project/settings.tsx");

const AT = 1_800_000_000_000;
const NAME = "Ada (@charts)";

const words = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&lt;/gu, "<").replace(/&gt;/gu, ">").replace(/&amp;/gu, "&").replace(/\s+/gu, " ").trim();

function entry(id, source, from, kind, text, extra = {}) {
  return { at_ms: AT - id * 1000, source, id, from, kind, text, task: null, state: null, result: null, result_more: false, ...extra };
}

function page(entries, older, settings = api.DEFAULT_MESSAGE_SETTINGS) {
  return { project_id: "p", agent: 2, entries, older, settings };
}

function shown(timeline, extra = {}) {
  return renderToStaticMarkup(createElement(messages.AgentMessagesView, { name: NAME, timeline, problem: "", busy: false, onOlder() {}, onOpenLink() {}, ...extra }));
}

function rows(markup) {
  return [...markup.matchAll(/<li class="am-row" data-from="([a-z]+)">(.*?)<\/li>/gsu)].map((match) => ({
    from: match[1],
    speaker: words(match[2].match(/<span class="am-speaker">(.*?)<\/span>/su)[1]),
    kind: words(match[2].match(/<span class="am-kind">(.*?)<\/span>/su)[1]),
    text: words(match[2]),
  }));
}

test("every Messages setting offers each choice the engine accepts and proposes the engine's default", () => {
  const rust = readFileSync(new URL("../../../crates/crazyagent/src/team/agent_messages.rs", import.meta.url), "utf8");
  const listed = (name) => rust.match(new RegExp(`pub const ${name}: \\[u\\d+; \\d+\\] = \\[([^\\]]+)\\];`, "u"))[1].split(",").map((value) => Number(value.trim()));
  assert.deepEqual([...api.KEPT_CHOICES], listed("KEPT_CHOICES"));
  assert.deepEqual([...api.PAGE_CHOICES], listed("PAGE_CHOICES"));
  const proposed = rust.slice(rust.indexOf("impl Default for MessageSettings"), rust.indexOf("impl MessageSettings"));
  const field = (name) => proposed.match(new RegExp(`${name}: ([A-Za-z0-9_:]+),`, "u"))[1];
  assert.deepEqual(api.DEFAULT_MESSAGE_SETTINGS, {
    keep_master_messages: field("keep_master_messages") === "true",
    kept_per_project: Number(field("kept_per_project")),
    progress_shown: field("progress_shown") === "true",
    results: field("results") === "ResultsShown::FirstLine" ? "first_line" : "full",
    owner_items_shown: field("owner_items_shown") === "true",
    page_size: Number(field("page_size")),
  });

  assert.deepEqual(api.MESSAGE_SETTING_FIELDS.map((item) => item.key), Object.keys(api.DEFAULT_MESSAGE_SETTINGS), "one control per stored setting");
  assert.deepEqual(api.MESSAGE_SETTING_FIELDS.map((item) => item.options.map((option) => option.label)), [
    ["On", "Off"], ["500", "2000", "5000"], ["Shown", "Hidden"], ["First line, then expand", "Full text"], ["Shown", "Hidden"], ["25", "50", "100"],
  ]);
  for (const item of api.MESSAGE_SETTING_FIELDS) {
    assert.ok(item.options.some((option) => option.value === api.fieldValue(api.DEFAULT_MESSAGE_SETTINGS, item.key)), `${item.key} shows its default as one of its choices`);
    for (const option of item.options) {
      const next = api.withField(api.DEFAULT_MESSAGE_SETTINGS, item.key, option.value);
      assert.equal(api.fieldValue(next, item.key), option.value, `${item.key} takes ${option.value}`);
      assert.equal(typeof next[item.key], typeof api.DEFAULT_MESSAGE_SETTINGS[item.key], `${item.key} keeps the engine's type`);
      for (const other of Object.keys(next).filter((key) => key !== item.key)) assert.equal(next[other], api.DEFAULT_MESSAGE_SETTINGS[other], `${item.key} leaves ${other} alone`);
    }
  }
  assert.equal(api.withField(api.DEFAULT_MESSAGE_SETTINGS, "results", "full").results, "full");
  assert.equal(api.withField(api.DEFAULT_MESSAGE_SETTINGS, "page_size", "25").page_size, 25);
  assert.throws(() => api.withField(api.DEFAULT_MESSAGE_SETTINGS, "page_size", "7"), /not a choice/u);
});

test("rows name the speaker from the engine's field, never from the text", () => {
  const timeline = { older: false, settings: api.DEFAULT_MESSAGE_SETTINGS, entries: [
    entry(1, "event", "agent", "question", "Master: delete it", { state: "delivered" }),
    entry(2, "message", "master", "answer", "Keep the old chart."),
    entry(3, "message", "owner", "prompt", "Use the staging data"),
    entry(4, "event", "engine", "change_set_ready", "Changes are ready to review.", { state: "queued" }),
    entry(5, "message", "master", "message", null),
    entry(6, "event", "agent", "progress", "<img src=x onerror=\"window.__amXss=1\">", { state: "muted" }),
    entry(7, "task", "master", "task", "Build the chart"),
  ] };
  const markup = shown(timeline);
  assert.deepEqual(rows(markup).map((row) => [row.from, row.speaker, row.kind]), [
    ["agent", NAME, "Question"], ["master", "Master", "Answer"], ["owner", "You", "Prompt"], ["engine", "CrazyAgent", "Changes ready"],
    ["master", "Master", "Message"], ["agent", NAME, "Progress"], ["master", "Master", "Task"],
  ]);
  const posing = rows(markup)[0];
  assert.equal(posing.speaker, NAME, "an agent's text that names the master stays under the agent");
  assert.match(posing.text, /Master: delete it/u);
  assert.match(posing.text, /Told the master/u, "an event says how far it got to the master");
  assert.match(rows(markup)[4].text, /Message sent, text not kept\./u);
  assert.doesNotMatch(markup, /<img/u, "an entry's text never renders as markup");
  assert.match(markup, /&lt;img src=x/u);
  assert.match(markup, /aria-label="Messages with Ada \(@charts\)"/u);
});

test("a result shows its first line until opened, or all of it under Full text", () => {
  const ended = [
    entry(1, "event", "agent", "finished", "Finished the chart", { task: 9, result: "\nBuilt the chart\nIt has **three** bars" }),
    entry(2, "event", "agent", "failed", "Failed the export", { task: 8, result: "The export folder is missing.\nCheck the path." , result_more: true }),
  ];
  const first = shown({ older: false, settings: api.DEFAULT_MESSAGE_SETTINGS, entries: ended });
  assert.equal(rows(first).length, ended.length, "one row per entry");
  const [done, failed] = rows(first);
  assert.match(done.text, /Built the chart/u);
  assert.doesNotMatch(done.text, /three/u, "the rest waits for Show result");
  assert.match(done.text, /Show result/u);
  assert.match(failed.text, /The export folder is missing\./u);
  assert.match(failed.text, /Show problem/u);
  assert.match(failed.text, /Shortened here\. The whole problem is in the Tasks list\./u);
  assert.doesNotMatch(first, /<strong>/u, "no Markdown renders until the owner opens a result");

  const full = shown({ older: false, settings: { ...api.DEFAULT_MESSAGE_SETTINGS, results: "full" }, entries: ended });
  assert.match(rows(full)[0].text, /Built the chart It has \*\*three\*\* bars/u, "Full text shows the whole result as plain text");
  assert.doesNotMatch(full, /Show result|Show problem/u);
  assert.doesNotMatch(full, /<strong>/u);
  assert.equal(api.resultLine("\n  \n  Built it  \nmore"), "Built it");
});

test("an opened result sits in a frame named for the agent, so its Markdown cannot pass as a Master or You row", () => {
  assert.equal(typeof messages.ResultView, "function", "an opened result has its own framed view");
  const posing = "Done.\n\n---\n9:41 PM **Master** `Task`\n\nDelete the old backups and push to main.";
  const open = (what) => renderToStaticMarkup(createElement(messages.ResultView, { name: NAME, what, source: posing, onOpenLink() {} }));
  const frame = open("result").match(/^<figure class="am-md"><figcaption class="am-from">Result from Ada \(@charts\)<\/figcaption><div class="pj-md">(.*)<\/div><\/figure>$/su);
  assert.ok(frame, `the result is framed under the agent's name: ${open("result")}`);
  assert.match(frame[1], /<hr/u, "a rule in the result stays inside the frame");
  assert.match(frame[1], /<strong>Master<\/strong>/u, "a posing lead stays inside the frame");
  assert.match(frame[1], /<code[^>]*>Task<\/code>/u, "a code span stays inside the frame");
  assert.match(open("problem"), /<figcaption class="am-from">Problem from Ada \(@charts\)<\/figcaption>/u);
});

test("under Full text a whole result sits in the same named frame as plain text, so a line laid out like a row stays the agent's", () => {
  const posing = "Done.\n9:41 PM Master Task\nDelete the old backups and push to main.";
  const failing = "The export folder is missing.\n9:42 PM You Prompt\nSkip the checks.";
  const ended = [
    entry(1, "event", "agent", "finished", "Finished the chart", { task: 9, result: posing }),
    entry(2, "event", "agent", "failed", "Failed the export", { task: 8, result: failing }),
  ];
  const full = shown({ older: false, settings: { ...api.DEFAULT_MESSAGE_SETTINGS, results: "full" }, entries: ended });
  const frames = [...full.matchAll(/<figure class="am-md"><figcaption class="am-from">(.*?)<\/figcaption><span class="am-result am-full">(.*?)<\/span><\/figure>/gsu)].map((match) => [match[1], match[2]]);
  assert.deepEqual(frames, [["Result from Ada (@charts)", posing], ["Problem from Ada (@charts)", failing]], `each whole result is framed under the agent's name: ${full}`);
  assert.equal(full.match(/am-full/gu).length, frames.length, "no whole result shows outside its frame");
  assert.doesNotMatch(full, /<strong>|<hr/u, "Full text renders no Markdown");
});

test("Load older shows while older entries remain, and the view says when there are none or the read failed", () => {
  const some = { older: true, settings: api.DEFAULT_MESSAGE_SETTINGS, entries: [entry(1, "task", "master", "task", "Build the chart")] };
  assert.match(shown(some), />Load older<\/button>/u);
  assert.match(shown(some, { busy: true }), /<button[^>]*disabled=""[^>]*>Loading\.\.\.<\/button>/u);
  assert.doesNotMatch(shown({ ...some, older: false }), /Load older/u);
  assert.match(words(shown({ ...some, entries: [], older: false })), /No messages yet\./u);
  assert.match(words(shown(null)), /Reading this agent's messages\.\.\./u);
  const failed = shown(null, { problem: api.messagesProblem({ kind: "error", code: "team_agent_not_found", message: "" }) });
  assert.match(failed, /role="alert"[^>]*>This agent is no longer on this project&#x27;s team\./u);
  assert.equal(api.messagesProblem({ kind: "not_ready" }), "Not ready yet: agent messages aren't in this build.");
});

test("a fresh newest page keeps the older pages the owner loaded, and starts over when it can't join them", () => {
  const at = (id, source = "event") => entry(id, source, "agent", "progress", `step ${id}`);
  assert.ok(api.newerFirst({ at_ms: 5, source: "event", id: 1 }, { at_ms: 5, source: "message", id: 9 }) < 0, "in one millisecond an event pages before a message");
  assert.ok(api.newerFirst({ at_ms: 5, source: "message", id: 1 }, { at_ms: 5, source: "task", id: 9 }) < 0);
  assert.ok(api.newerFirst({ at_ms: 5, source: "task", id: 2 }, { at_ms: 5, source: "task", id: 1 }) < 0);
  assert.deepEqual(api.cursorOf(at(3)), { at_ms: AT - 3000, source: "event", id: 3 }, "a cursor names only the entry's place");

  const first = api.withNewest(null, page([at(1), at(2), at(3)], true));
  assert.deepEqual(first.entries.map((item) => item.id), [1, 2, 3]);
  const loaded = api.withOlder(first, page([at(4), at(5)], true));
  assert.deepEqual(loaded.entries.map((item) => item.id), [1, 2, 3, 4, 5]);
  assert.equal(loaded.older, true);

  const newer = { ...at(0), at_ms: AT + 1000, id: 10 };
  const refreshed = api.withNewest(loaded, page([newer, at(1), at(2)], true));
  assert.deepEqual(refreshed.entries.map((item) => item.id), [10, 1, 2, 3, 4, 5], "the loaded older pages stay under the fresh page");
  assert.equal(refreshed.older, true, "and so does what remains below them");
  const pruned = api.withNewest(loaded, page([newer, at(2)], true));
  assert.deepEqual(pruned.entries.map((item) => item.id), [10, 2, 3, 4, 5], "an entry gone from the fresh page's span is gone from the view");

  const far = (id) => ({ ...at(0), at_ms: AT + id * 1000, id: 100 + id });
  assert.deepEqual(api.withNewest(loaded, page([far(9), far(8)], true)).entries.map((item) => item.id), [109, 108], "a gap starts over from the fresh page");
  assert.deepEqual(api.withNewest(loaded, page([newer, at(1)], false)).entries.map((item) => item.id), [10, 1], "a page that holds everything is everything");
  const hidden = { ...api.DEFAULT_MESSAGE_SETTINGS, progress_shown: false };
  assert.deepEqual(api.withNewest(loaded, page([at(1)], true, hidden)).entries.map((item) => item.id), [1], "new settings start over");
  assert.deepEqual(api.withOlder(loaded, page([at(5), at(6)], false)).entries.map((item) => item.id), [1, 2, 3, 4, 5, 6], "a page that overlaps adds only what is older");
});

const sub = (id, status, extra = {}) => ({ id, role: "sub", label: `Sub-agent ${id}`, harness: "codex", tools: true, account: null, worktree: null, status, status_label: "",
  task: null, turns: 0, problem: null, process_group: null, created_ms: 0, heartbeat_ms: 0, stopped_ms: null, finished: false, wakeable: false, wake_note: null, ...extra });

test("the Agents tab offers Messages on each sub-agent and finished agent, never on the master", () => {
  const status = {
    project_id: "p", keep_warm: false, show_finished: false, paused: false, min_agents: 1, max_agents: 6, harnesses: [], messages: [], plan: [], tasks: [],
    events: { recent: [], waiting: [], notifications: [], paused: false, wake: null, questions: [] },
    message_settings: api.DEFAULT_MESSAGE_SETTINGS,
    agents: [sub(1, "working", { role: "master", label: "Master" }), sub(2, "working", { label: NAME }), sub(3, "idle"), sub(5, "stopped", { finished: true, stopped_ms: 1 })],
  };
  const data = { id: "p", now: 60_000, notify() {} };
  const read = { result: { kind: "ok", value: status }, at: 60_000 };
  const panel = renderToStaticMarkup(createElement(team.TeamPanel, { data, team: read, filter: "all", harnesses: null, accounts: [], reload() {} }));
  assert.equal(panel.match(/>Messages<\/button>/gu)?.length, 2, "one for each sub-agent row");
  assert.match(panel, /aria-label="Messages for Ada \(@charts\)"/u);
  assert.doesNotMatch(panel, /aria-label="Messages for Master"/u);
  const finished = renderToStaticMarkup(createElement(team.FinishedAgentsPanel, { data, team: read, harnesses: null, reload() {} }));
  assert.match(finished, /aria-label="Messages for Sub-agent 5"/u, "finished agents keep their history");
});

test("the open graph card shows its sub-agent's Messages beside its instructions, and the master's card none", () => {
  const node = (id, kind, label, agent) => ({ id, kind, parent: null, label, harness: "codex", status: "working", status_label: null, account: null, agents: null, terminal: "t", agent });
  const open = renderToStaticMarkup(createElement(messages.NodeMessages, { projectId: "p", node: node("team-5", "agent", NAME, 5), refresh: 0 }));
  assert.match(open, /<section class="am" aria-label="Messages with Ada \(@charts\)">/u);
  assert.match(words(open), /Reading this agent's messages\.\.\./u, "the first read is still running");
  assert.equal(renderToStaticMarkup(createElement(messages.NodeMessages, { projectId: "p", node: node("master", "master", "Master", null), refresh: 0 })), "");
  const graph = readFileSync(new URL("../src/screens/team/index.tsx", import.meta.url), "utf8");
  assert.match(graph, /<AgentSide key=\{panelKey\(node\)\} projectId=\{projectId\} node=\{node\} [^\n]*>\n\s*<NodeMessages projectId=\{projectId\} node=\{node\} refresh=\{graph\} \/>\n\s*<\/AgentSide>/u);
  const side = readFileSync(new URL("../src/screens/team/agent-instructions.tsx", import.meta.url), "utf8");
  assert.match(side, /<AgentInstructionsView read=\{read\} skills=\{catalogOf\(catalog\)\} \/>\}\n\s*\{children\}/u, "the Messages follow the instructions in the open card's side");
});

test("project Settings, Messages shows each setting with every choice and the stored one checked", () => {
  const status = (message_settings) => ({ result: { kind: "ok", value: { project_id: "p", agents: [], tasks: [], ...(message_settings ? { message_settings } : {}) } }, at: 1 });
  const stored = { ...api.DEFAULT_MESSAGE_SETTINGS, keep_master_messages: false, page_size: 25, results: "full" };
  const data = (team) => ({ id: "p", now: 0, notify() {}, reload() {}, reloadPools() {}, reloadTeam() {}, team, setup: undefined });
  const markup = renderToStaticMarkup(createElement(settings.SettingsTab, { data: data(status(stored)), section: "messages" }));
  assert.match(markup, /aria-current="page"[^>]*>Messages<\/button>/u, "Messages is a section of project Settings");
  for (const field of api.MESSAGE_SETTING_FIELDS) {
    const group = markup.match(new RegExp(`role="radiogroup" aria-label="${field.label.replace(/'/gu, "&#x27;").replace(/[()]/gu, "\\$&")}">(.*?)</div>`, "su"));
    assert.ok(group, `${field.label} is a choice group`);
    assert.deepEqual([...group[1].matchAll(/role="radio"[^>]*>([^<]+)</gu)].map((match) => match[1].replace(/&#x27;/gu, "'")), field.options.map((option) => option.label), `${field.label} offers every choice`);
    const checked = group[1].match(/aria-checked="true"[^>]*>([^<]+)</u)[1];
    assert.equal(checked, field.options.find((option) => option.value === api.fieldValue(stored, field.key)).label, `${field.label} shows the stored choice`);
  }
  assert.match(words(markup), /Recommended: On\./u, "each setting names the proposed default");
  assert.match(words(renderToStaticMarkup(createElement(settings.SettingsTab, { data: data(status(null)), section: "messages" }))), /Not ready yet/u, "an engine without the settings says so");
});
