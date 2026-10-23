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
  assert.deepEqual(research.researchRow(job("queued"), now), { title: "Market research", status: "Queued", tone: "info", progress: "Waiting to start", elapsed: "", report: false, active: true });
  assert.deepEqual(research.researchRow(job("needs_pool"), now), { title: "Market research", status: "Needs a pool", tone: "warn", progress: "Waiting for a pool", elapsed: "", report: false, active: false });
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
  assert.equal(research.shouldPoll([job("needs_pool")]), false);
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
  assert.equal(research.activity(Array.from({ length: 10 }, (_, i) => job("running", { id: i, started_ms: 2_000 + i })), "x", 1).length, 8);
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
