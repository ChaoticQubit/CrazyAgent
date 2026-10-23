import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");
const api = await import("../src/api-projects.ts");

const job = (status, extra = {}) => ({
  id: 1, project_id: "p", kind: "market_research", kind_label: "Market research", status, status_label: "", report_path: "context/research/market_research.md",
  harness: "codex", tools: true, attempts: 0, claimer: null, error: null, cancel_requested: false, output_bytes: 0,
  created_ms: 1_000, started_ms: null, heartbeat_ms: null, finished_ms: null, ...extra,
});

test("elapsed time reads in seconds, minutes and hours", () => {
  assert.equal(research.elapsed(0), "0 s");
  assert.equal(research.elapsed(45_000), "45 s");
  assert.equal(research.elapsed(18 * 60_000 + 20_000), "18 min");
  assert.equal(research.elapsed(2 * 3_600_000 + 5 * 60_000), "2 h 5 min");
  assert.equal(research.elapsed(-5), "0 s");
});

test("research rows use the fixed status words and only real progress", () => {
  const now = 1_000 + 18 * 60_000;
  assert.deepEqual(research.researchRow(job("queued"), now), { title: "Market research", status: "Queued", tone: "neutral", progress: "Waiting to start", elapsed: "", report: false, active: true });
  assert.deepEqual(research.researchRow(job("needs_pool"), now), { title: "Market research", status: "Needs a pool", tone: "warn", progress: "Waiting for a pool", elapsed: "", report: false, active: false });
  assert.deepEqual(research.researchRow(job("needs_pool", { error: "No account in this project's pool can run this research" }), now),
    { title: "Market research", status: "Needs a pool", tone: "warn", progress: "No account in this project's pool can run this research", elapsed: "", report: false, active: false });
  assert.equal(research.researchRow(job("needs_pool", { error: "  " }), now).progress, "Waiting for a pool");
  assert.deepEqual(research.researchRow(job("running", { started_ms: 1_000, output_bytes: 2048 }), now),
    { title: "Market research", status: "Running", tone: "info", progress: "Writing report, 2 KB so far", elapsed: "18 min", report: false, active: true });
  assert.equal(research.researchRow(job("running", { started_ms: 1_000 }), now).progress, "Working");
  assert.deepEqual(research.researchRow(job("done", { started_ms: 1_000, finished_ms: 1_000 + 26 * 60_000 }), now),
    { title: "Market research", status: "Done", tone: "ok", progress: "Report saved", elapsed: "26 min", report: true, active: false });
  assert.deepEqual(research.researchRow(job("failed", { error: "Agent ran out of time" }), now).progress, "Agent ran out of time");
  assert.equal(research.researchRow(job("failed"), now).progress, "Stopped with an error.");
  assert.equal(research.researchRow(job("failed"), now).status, "Failed");
  assert.equal(research.researchRow(job("cancelled"), now).status, "Stopped");
  assert.equal(research.researchRow(job("running", { cancel_requested: true, started_ms: 1_000 }), now).progress, "Stopping");
});

test("research polling continues only while something can still change", () => {
  assert.equal(research.shouldPoll([job("done"), job("failed")]), false);
  assert.equal(research.shouldPoll([job("done"), job("running")]), true);
  assert.equal(research.shouldPoll([job("queued")]), true);
  assert.equal(research.shouldPoll([job("needs_pool")]), true);
  assert.equal(research.shouldPoll([job("cancelled"), job("done")]), false);
  assert.equal(research.shouldPoll([]), false);
});

test("recent activity is built from real job times, newest first, at most eight", () => {
  const jobs = [
    job("done", { kind_label: "Market research", started_ms: 2_000, finished_ms: 5_000, report_path: "context/research/market_research.md" }),
    job("running", { id: 2, kind_label: "Design research", started_ms: 3_000 }),
    job("failed", { id: 3, kind_label: "Competitor research", started_ms: 2_500, finished_ms: 4_000, error: "Agent output exceeded the limit" }),
  ];
  const activity = research.activity(jobs, "Harbor Notes", 1_000);
  assert.deepEqual(activity.map((item) => item.text), [
    "Market research saved market_research.md",
    "Competitor research stopped: Agent output exceeded the limit",
    "Design research started",
    "Competitor research started",
    "Market research started",
    "You created Harbor Notes with 3 research tasks",
  ]);
  const many = research.activity(Array.from({ length: 10 }, (_, i) => job("running", { id: i, started_ms: 2_000 + i })), "x", 1);
  assert.equal(many.length, 11);
  assert.equal(many[0].at, 2_009);
  assert.equal(many[10].at, 1);
  assert.equal(research.ACTIVITY_ROWS, 8);
});

test("relative time reads like the design", () => {
  const now = 10 * 86_400_000;
  assert.equal(research.ago(now - 20_000, now), "Just now");
  assert.equal(research.ago(now - 12 * 60_000, now), "12 min ago");
  assert.equal(research.ago(now - 2 * 3_600_000, now), "2 h ago");
  assert.equal(research.ago(now - 30 * 3_600_000, now), "Yesterday");
  assert.equal(research.ago(now - 4 * 86_400_000, now), "4 days ago");
});

test("home folder paths are shown with a tilde", () => {
  assert.equal(research.tildePath("/Users/mira/CrazyAgent/projects/x", "/Users/mira"), "~/CrazyAgent/projects/x");
  assert.equal(research.tildePath("/opt/x", "/Users/mira"), "/opt/x");
  assert.equal(research.tildePath("/Users/mirage/x", "/Users/mira"), "/Users/mirage/x");
  assert.equal(research.homePath("/Users/mira/Docs/field-interviews.md"), "~/Docs/field-interviews.md");
  assert.equal(research.homePath("/Users/mira"), "~");
  assert.equal(research.homePath("/Volumes/Work/notes.md"), "/Volumes/Work/notes.md");
});

test("master status words follow the design's fixed set", () => {
  const turn = (phase, reason = null) => ({ id: "t", ordinal: 1, phase, reason, reply: null });
  assert.equal(research.masterStatus(null, []), "Ready");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("running")]), "Thinking...");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("succeeded")]), "Replied");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("blocked", "readiness_unknown")]), "Not signed in");
  assert.equal(research.masterStatus({ session_id: "s", phase: "closed", current_turn: null }, [turn("succeeded")]), "Stopped");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("failed", "provider_error")]), "Stopped");
  assert.equal(research.turnLine(turn("blocked", "readiness_stale")), "Sign-in is out of date. Sign in again in Settings, Accounts, then send again.");
  assert.equal(research.turnLine(turn("failed", "provider_error")), "The master stopped: provider error.");
  assert.equal(research.turnLine(turn("queued")), "Waiting to start");
});

test("unassigning a pool removes only this project", () => {
  const pool = { id: "main", members: ["a"], projects: ["p-1", "p-2"], roles: ["project_leader"], fallback_allowed: false };
  assert.deepEqual(api.unassignedPool(pool, "p-1"), { ...pool, projects: ["p-2"] });
  assert.deepEqual(api.unassignedPool(pool, "p-9"), pool);
});

test("a project page for an unknown engine renders its loading shell without bridge effects", async () => {
  const page = await import("../src/screens/project/index.tsx");
  const markup = renderToStaticMarkup(createElement(page.ProjectPage, { id: "p-1", tab: "overview" }));
  assert.match(markup, /role="tablist"/);
  for (const tab of ["Overview", "Conversation", "Agents", "Context", "Settings"]) assert.match(markup, new RegExp(`>${tab}<`));
  assert.match(markup, /id="tab-overview" role="tab" class="tab" href="#\/projects\/p-1\/overview" aria-selected="true" tabindex="0"><svg[\s\S]*?<\/svg>Overview</);
  assert.match(markup, /role="tabpanel" aria-labelledby="tab-overview"/);
  assert.doesNotMatch(markup, /<h1><div/);
  assert.doesNotMatch(markup, /Harbor Notes|Market research suggests/);
});

test("the New Project flow renders Basics first with labelled fields", async () => {
  const flow = await import("../src/screens/new-project/index.tsx");
  const markup = renderToStaticMarkup(createElement(flow.NewProjectFlow, {}));
  assert.match(markup, /<h1>New project<\/h1>/);
  assert.match(markup, /<label for="np-name">Project name<\/label>/);
  assert.match(markup, /<label for="np-about">What is it about\?<\/label>/);
  assert.match(markup, /<legend>Tech stack<\/legend>/);
  assert.match(markup, /<legend>Target platforms<\/legend>/);
  assert.match(markup, /aria-current="step"[^>]*>.*Basics/);
  for (const label of ["Website", "Mobile app", "Desktop app", "CLI", "API or backend", "Other"]) assert.match(markup, new RegExp(`>${label}</label>`));
  assert.doesNotMatch(markup, /role="alert"/);
});

test("saving one section cuts the same text the engine saves", async () => {
  const context = await import("../src/screens/project/context.tsx");
  const report = "# Market\nintro\n## Who pays\nOwners pay.\n### Detail\nmore\n## Size\nbig\n#nothashtag\n";
  assert.equal(context.sectionText(report, null), report.trim());
  assert.equal(context.sectionText(report, "Who pays"), "## Who pays\nOwners pay.\n### Detail\nmore");
  assert.equal(context.sectionText(report, " Size "), "## Size\nbig\n#nothashtag");
  assert.equal(context.sectionText(report, "Missing"), null);
  assert.equal(context.researchKindOf("context/research/market_research.md", [{ kind: "market_research", report_path: "context/research/market_research.md" }]), "market_research");
  assert.equal(context.researchKindOf("context/brief.md", [{ kind: "market_research", report_path: "context/research/market_research.md" }]), null);
});

test("agent rows use the design's agent words", async () => {
  const agents = await import("../src/screens/project/agents.tsx");
  assert.deepEqual(agents.agentStatus({ status: "Queued", tone: "info" }), { word: "Starting", tone: "info" });
  assert.deepEqual(agents.agentStatus({ status: "Running", tone: "info" }), { word: "Running", tone: "info" });
  assert.deepEqual(agents.agentStatus({ status: "Stopped", tone: "neutral" }), { word: "Stopped", tone: "neutral" });
});

test("project folders under the home folder show a tilde", async () => {
  const overview = await import("../src/screens/project/overview.tsx");
  assert.equal(overview.folderLabel("/Users/mira/Library/CrazyAgent/projects/p-1"), "~/Library/CrazyAgent/projects/p-1");
  assert.equal(overview.folderLabel("/private/var/p-1"), "/private/var/p-1");
  const id = "project-3895cf3bd08cc3079f7602b664bff2aba118e9468c086704878b37cdd0555e03";
  assert.equal(overview.folderLabel(`/Users/mira/Library/CrazyAgent/projects/${id}`), "~/Library/CrazyAgent/projects/project-3895cf3b...");
  assert.doesNotMatch(overview.folderLabel(`/Users/mira/${id}/projects/${id}`), /[0-9a-f]{12}/);
  assert.equal(overview.folderLabel("/Users/mira/Projects/a-folder-name-of-exactly-32-chars"), "~/Projects/a-folder-name-of-exactly-32-chars");
});

test("context files get an icon for their type, and unknown types a plain file", () => {
  assert.equal(research.fileIcon("context/brief.md"), "file-text");
  assert.equal(research.fileIcon("context/documents/notes.TXT"), "file-text");
  assert.equal(research.fileIcon("context/documents/hours.csv"), "file-spreadsheet");
  assert.equal(research.fileIcon("context/documents/deck.png"), "file-image");
  assert.equal(research.fileIcon("context/documents/photo.jpeg"), "file-image");
  assert.equal(research.fileIcon("context/documents/spec.pdf"), "file");
  assert.equal(research.fileIcon("context/documents/README"), "file");
});

test("project memories split into Suggested and Saved and hide what the owner threw away", () => {
  const item = (id, status, human_reviewed = false) => ({ id, version: 1, text: `fact ${id}`, status, human_reviewed });
  const groups = research.memoryGroups([
    item("m1", "Provisional"), item("m2", "Confirmed", true), item("m3", "Rejected", true),
    item("m4", "Removed", true), item("m5", "Retracted"), item("m6", "Provisional"), item("m7", "Confirmed", false),
  ]);
  assert.deepEqual(groups.suggested.map((entry) => entry.id), ["m1", "m6"]);
  assert.deepEqual(groups.saved.map((entry) => entry.id), ["m2", "m7"]);
  assert.deepEqual(research.memoryGroups([]), { suggested: [], saved: [] });
});

test("deleting a context file uses the engine path and never the brief", () => {
  assert.equal(research.deletable({ path: "context/research/market_research.md", kind: "research", bytes: 1 }), true);
  assert.equal(research.deletable({ path: "context/documents/hours.csv", kind: "document", bytes: 1 }), true);
  assert.equal(research.deletable({ path: "context/brief.md", kind: "brief", bytes: 1 }), false);
  assert.equal(research.deleteQuestion("context/research/market_research.md"), "Delete market_research.md?");
});

test("the Needs you line for suggested memories counts them in plain words", () => {
  assert.equal(research.suggestedLine(1), "1 suggested memory waits for you to save or dismiss it.");
  assert.equal(research.suggestedLine(3), "3 suggested memories wait for you to save or dismiss them.");
});

test("the Needs you line for waiting research uses the engine's reason when it gives one", () => {
  assert.equal(research.waitingLine([job("needs_pool")]), "Research is waiting for a pool.");
  assert.equal(research.waitingLine([]), "Research is waiting for a pool.");
  assert.equal(research.waitingLine([job("done"), job("needs_pool", { error: "No account in this project's pool can run this research" })]),
    "Research is waiting: No account in this project's pool can run this research.");
  assert.equal(research.waitingLine([job("needs_pool", { error: "Needs a pool." })]), "Research is waiting: Needs a pool.");
});

const tabData = (jobs) => ({
  id: "p-1", name: "Harbor Notes", setup: undefined, research: { kind: "ok", value: { project_id: "p-1", jobs } }, context: undefined,
  pools: undefined, harnesses: undefined, assigned: null, now: 1_000 + 60_000, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
});

test("an agent that started offers View log, only a running one offers Stop", async () => {
  const agents = await import("../src/screens/project/agents.tsx");
  const running = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tabData([job("running", { started_ms: 1_000 })]) }));
  assert.match(running, /<button type="button" class="btn" aria-expanded="false" aria-controls="pj-log-1">View log<\/button>/);
  assert.match(running, />Stop</);
  assert.doesNotMatch(running, /id="pj-log-1"/);
  const queued = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tabData([job("queued")]) }));
  assert.doesNotMatch(queued, />View log<|>Stop</);
});

test("a failed research row shows its plain reason and Try again", async () => {
  const overview = await import("../src/screens/project/overview.tsx");
  const markup = renderToStaticMarkup(createElement(overview.OverviewTab, { data: tabData([job("failed", { error: "This harness cannot run research yet", started_ms: 1_000, finished_ms: 2_000 }), job("done")]) }));
  assert.match(markup, /This harness cannot run research yet/);
  assert.equal(markup.match(/>Try again</g)?.length, 1);
});

test("a stopped or failed master turn offers its own message to send again", () => {
  const turn = (ordinal, phase) => ({ id: `t${ordinal}`, ordinal, phase, reason: null, reply: phase === "succeeded" ? { text: "hi" } : null });
  assert.equal(research.resendText([turn(1, "succeeded"), turn(2, "failed")], { 1: "first", 2: "second" }), "second");
  assert.equal(research.resendText([turn(1, "cancelled")], { 1: "first" }), "first");
  assert.equal(research.resendText([turn(2, "blocked"), turn(1, "succeeded")], { 1: "a", 2: "b" }), "b");
  assert.equal(research.resendText([turn(1, "failed"), turn(2, "succeeded")], { 1: "first", 2: "second" }), null);
  assert.equal(research.resendText([turn(1, "running")], { 1: "first" }), null);
  assert.equal(research.resendText([turn(1, "failed")], {}), null);
  assert.equal(research.resendText([], {}), null);
});

test("a failed send says Couldn't send and keeps specific engine copy", () => {
  assert.equal(research.sendFailureLine("made_up_code"), "Couldn't send. Try again.");
  assert.equal(research.sendFailureLine("engine_unreachable"), `Couldn't send. ${api.failureCopy("engine_unreachable")}`);
});

test("a log follows new lines only while the owner is near its bottom", () => {
  assert.equal(research.nearBottom({ scrollHeight: 1000, scrollTop: 760, clientHeight: 240 }), true);
  assert.equal(research.nearBottom({ scrollHeight: 1000, scrollTop: 700, clientHeight: 240 }), true);
  assert.equal(research.nearBottom({ scrollHeight: 1000, scrollTop: 600, clientHeight: 240 }), false);
  assert.equal(research.nearBottom({ scrollHeight: 100, scrollTop: 0, clientHeight: 240 }), true);
});

test("Recent activity shows eight rows and Show all for the rest", async () => {
  const overview = await import("../src/screens/project/overview.tsx");
  const jobs = Array.from({ length: 5 }, (_, i) => job("done", { id: i, kind_label: `Kind ${i}`, started_ms: 2_000 + i, finished_ms: 3_000 + i }));
  const markup = renderToStaticMarkup(createElement(overview.OverviewTab, { data: tabData(jobs) }));
  assert.equal(markup.match(/class="pj-activity-row"/g)?.length, 8);
  assert.match(markup, />Show all \(10\)</);
  const few = renderToStaticMarkup(createElement(overview.OverviewTab, { data: tabData(jobs.slice(0, 2)) }));
  assert.equal(few.match(/class="pj-activity-row"/g)?.length, 4);
  assert.doesNotMatch(few, />Show all/);
});

test("Thinking shows the seconds since this window sent the message", () => {
  assert.equal(research.thinkingLine(undefined, 50_000), "Thinking...");
  assert.equal(research.thinkingLine(38_000, 50_000), "Thinking... 12 s");
  assert.equal(research.thinkingLine(50_000, 49_000), "Thinking... 0 s");
  assert.equal(research.thinkingLine(0, 3 * 60_000 + 5_000), "Thinking... 3 min");
});

test("Stop targets the master turn that is still replying", () => {
  const turn = (ordinal, phase) => ({ id: `t${ordinal}`, ordinal, phase, reason: null, reply: null });
  assert.equal(research.replyingTurn([turn(1, "succeeded"), turn(2, "running")])?.id, "t2");
  assert.equal(research.replyingTurn([turn(3, "queued"), turn(1, "failed")])?.id, "t3");
  assert.equal(research.replyingTurn([turn(1, "succeeded"), turn(2, "cancelled")]), null);
  assert.equal(research.replyingTurn([]), null);
});

test("the master's harness that is not installed gets the design's line", () => {
  const entry = (id, name, kind) => ({ id, name, status: { kind }, status_label: "", executable: null, headless: "", tools: { kind: "switchable" }, model_selection: false, account_kinds: [] });
  const catalog = [entry("codex", "Codex", "not_installed"), entry("claude", "Claude Code", "ready")];
  assert.equal(research.masterMissingLine(catalog, "codex"), "Codex isn't installed on this Mac.");
  assert.equal(research.masterMissingLine(catalog, "claude"), null);
  assert.equal(research.masterMissingLine(catalog, "unknown"), null);
  assert.equal(research.masterMissingLine(null, "codex"), null);
});

test("only the brief offers Edit, which opens the project details it is written from", async () => {
  const context = await import("../src/screens/project/context.tsx");
  const folder = { kind: "ok", value: { project_id: "p-1", folder: "/tmp/p-1", items: [
    { path: "context/brief.md", kind: "brief", bytes: 120 },
    { path: "context/documents/notes.md", kind: "document", bytes: 40 },
  ] } };
  const brief = renderToStaticMarkup(createElement(context.ContextTab, { data: { ...tabData([]), context: folder } }));
  assert.match(brief, /<h2 class="pj-mono">context\/brief\.md<\/h2>/);
  assert.match(brief, />Edit</);
  const documentOnly = { kind: "ok", value: { ...folder.value, items: [folder.value.items[1]] } };
  const markup = renderToStaticMarkup(createElement(context.ContextTab, { data: { ...tabData([]), context: documentOnly } }));
  assert.doesNotMatch(markup, />Edit</);
});

test("Needs you names the project's pool accounts that aren't signed in", () => {
  const member = (id, status) => ({ id, kind: "chatgpt", provider: "openai", status, status_label: "" });
  const pool = (members) => ({ id: "pool-1", name: "Main pool", summary: "", members, project_count: 1 });
  assert.equal(research.signInLine(pool([member("a", "signed_in"), member("b", "not_signed_in")])), "1 account in Main pool isn't signed in.");
  assert.equal(research.signInLine(pool([member("a", "not_signed_in"), member("b", "not_signed_in"), member("c", "set_up")])), "2 accounts in Main pool aren't signed in.");
  assert.equal(research.signInLine(pool([member("a", "signed_in"), member("b", "set_up")])), null);
  assert.equal(research.signInLine(undefined), null);
});
