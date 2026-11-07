import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
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
  const requeued = "A toolchain this research ran with was turned off, so it was stopped and queued again; its next run goes without it";
  assert.deepEqual(research.researchRow(job("queued", { error: requeued, started_ms: 1_000 }), now),
    { title: "Market research", status: "Queued", tone: "neutral", progress: requeued, elapsed: "", report: false, active: true });
  assert.equal(research.researchRow(job("queued", { error: "  " }), now).progress, "Waiting to start");
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

test("research waiting on a rate limit reads Rate limited until its local end, not Needs a pool", () => {
  process.env.TZ = "America/New_York";
  const reason = "Every account in this project's pool that can run this research hit its rate limit";
  const limited = job("needs_pool", { status_label: "Rate limited", error: `rate_limited until 2026-10-23 04:32 UTC: ${reason}` });
  assert.deepEqual(research.researchRow(limited, 0),
    { title: "Market research", status: "Rate limited until 12:32 AM", tone: "warn", progress: reason, elapsed: "", report: false, active: false });
  assert.equal(research.waitingLine([job("done"), limited]), "Research is rate limited until 12:32 AM.");
  const malformed = job("needs_pool", { error: `rate_limited until soon: ${reason}` });
  assert.equal(research.researchRow(malformed, 0).status, "Needs a pool");
  assert.equal(research.researchRow(job("queued", { error: limited.error }), 0).status, "Queued");
});

test("research polling continues only while something can still change", () => {
  assert.equal(research.shouldPoll([job("done"), job("failed")]), false);
  assert.equal(research.shouldPoll([job("done"), job("running")]), true);
  assert.equal(research.shouldPoll([job("queued")]), true);
  assert.equal(research.shouldPoll([job("needs_pool")]), true);
  assert.equal(research.shouldPoll([job("cancelled"), job("done")]), false);
  assert.equal(research.shouldPoll([]), false);
});

test("relative time reads like the design", () => {
  const now = 10 * 86_400_000;
  assert.equal(research.ago(now - 20_000, now), "Just now");
  assert.equal(research.ago(now - 12 * 60_000, now), "12 min ago");
  assert.equal(research.ago(now - 2 * 3_600_000, now), "2 h ago");
  assert.equal(research.ago(now - 30 * 3_600_000, now), "Yesterday");
  assert.equal(research.ago(now - 4 * 86_400_000, now), "4 days ago");
});

test("project folders and attached documents are shown under the home folder with a tilde", async () => {
  const overview = await import("../src/screens/project/overview.tsx");
  const { homePath } = await import("../src/screens/settings/harnesses.tsx");
  assert.equal(overview.folderLabel("/Users/mira/CrazyAgent/projects/harbor-notes"), "~/CrazyAgent/projects/harbor-notes");
  assert.equal(overview.folderLabel("/opt/x"), "/opt/x");
  assert.equal(homePath("/Users/mira/Docs/field-interviews.md"), "~/Docs/field-interviews.md");
  assert.equal(homePath("/Users/mira"), "~");
  assert.equal(homePath("/Volumes/Work/notes.md"), "/Volumes/Work/notes.md");
});

test("master status words follow the design's fixed set", () => {
  const turn = (phase, reason = null) => ({ id: "t", ordinal: 1, phase, reason, reply: null });
  assert.equal(research.masterStatus(null, []), "Ready");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("running")]), "Thinking...");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("succeeded")]), "Replied");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("blocked", "readiness_unknown")]), "Not signed in");
  assert.equal(research.masterStatus({ session_id: "s", phase: "closed", current_turn: null }, [turn("succeeded")]), "Stopped");
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [turn("failed", "provider_error")]), "Stopped");
  assert.equal(research.turnLine(turn("blocked", "readiness_stale")), "Sign-in is out of date. Sign in again in Settings, Accounts, then send again. (readiness_stale)");
  assert.equal(research.turnLine(turn("failed", "provider_error")), "The master stopped. (provider_error)");
  assert.equal(research.turnLine(turn("blocked", "repository_missing")), "The master couldn't start: this project has no repository yet. (repository_missing)");
  assert.equal(research.resendText([{ ...turn("blocked", "repository_missing"), id: "t1", ordinal: 1 }], { t1: "first" }), "first");
  const refused = "/owner/repo has the Git setting core.hooksPath, which runs hooks from a folder the repository chooses. Remove it from the repository's .git/config, then send again.";
  assert.equal(research.turnLine({ ...turn("blocked", "repository_config_refused"), detail: refused }), `The master couldn't start: ${refused} (repository_config_refused)`);
  assert.equal(research.turnLine(turn("failed", "repository_config_refused")), "The master couldn't start: a Git setting in this project's repository runs commands or redirects Git. Remove it, then send again. (repository_config_refused)");
  assert.equal(research.turnLine(turn("queued")), "Waiting to start");
});

test("a stopped master turn shows a plain sentence and the cause the engine stored", () => {
  const turn = (phase, reason, cause, detail = null) => ({ id: "t", ordinal: 1, phase, reason, cause, detail, reply: null });
  const refused = "/owner/repo has the Git setting core.hooksPath, which runs hooks from a folder the repository chooses. Remove it from the repository's .git/config, then send again.";
  assert.equal(research.turnLine(turn("blocked", "runtime_unavailable", "rcli_profile_configuration_required")),
    "The master stopped: it could not start. Check it in Settings, Harnesses. (rcli_profile_configuration_required)");
  assert.equal(research.turnLine(turn("failed", "unknown", "master_runtime_binding")), "The master stopped. (master_runtime_binding)");
  assert.equal(research.turnLine(turn("blocked", "repository_config_refused", "repository_config_refused:core_hookspath", refused)),
    `The master couldn't start: ${refused} (repository_config_refused:core_hookspath)`);
  assert.equal(research.turnLine(turn("failed", "profile_busy", "native_profile_in_use")),
    "The master stopped: its account is busy signing in or out. (native_profile_in_use)");
  assert.equal(research.turnLine(turn("failed", "connection_changed", "native_profile_connection_missing")),
    "The master stopped: its settings changed while it was open. (native_profile_connection_missing)");
  assert.equal(research.turnLine(turn("failed", "transient_failure", "native_profile_renewal_failed")),
    "The master stopped: the account's sign-in could not be renewed just now, so it is unavailable for a moment. Your next message uses the next account in the pool, or this one after a minute. (native_profile_renewal_failed)");
  assert.equal(research.turnLine(turn("failed", "instructions_too_large", "master_instructions_too_large")),
    "The master stopped: the app and project instructions are too long for a Claude master. Shorten them on the Process page, then send again. (master_instructions_too_large)");
  for (const phase of ["failed", "blocked", "uncertain"]) {
    const line = research.turnLine(turn(phase, "unknown", null));
    assert.equal(line, `The master stopped. (${phase})`);
    assert.ok(!line.includes("unknown"), line);
  }
});

test("a master turn names its account, and a turn no account could run names each one and why", () => {
  const running = { id: "t2", ordinal: 2, phase: "running", reason: null, reply: null, attempt: { connection: { account_ref: "work-claude" } } };
  assert.deepEqual(research.accountLines(running), ["On account work-claude"]);
  assert.deepEqual(research.accountLines({ id: "t1", ordinal: 1, phase: "queued", reason: null, reply: null }), []);
  const blocked = {
    id: "t3", ordinal: 3, phase: "blocked", reason: "readiness_unknown", reply: null, attempt: null,
    accounts: [
      { account_ref: "work-claude", connection: "claude-a", harness: "claude", reason: "readiness_unknown" },
      { account_ref: "home-claude", connection: "claude-b", harness: "claude", reason: "rate_limited" },
      { account_ref: "spare-claude", connection: "claude-d", harness: "claude", reason: "transient_failure" },
      { account_ref: "work-codex", connection: "codex-c", harness: "codex", reason: null },
    ],
  };
  assert.equal(research.turnLine(blocked), "No account in this master's pool can run it now. (readiness_unknown)");
  assert.deepEqual(research.accountLines(blocked), [
    "work-claude: not signed in",
    "home-claude: rate limited, paused for a few minutes",
    "spare-claude: sign-in could not be renewed just now, available again in a minute",
    "work-codex is ready on codex: change the master to codex to use it.",
  ]);
  assert.equal(research.masterStatus({ session_id: "s", phase: "configured", current_turn: null }, [blocked]), "Not signed in");
});

test("a project page for an unknown engine renders its loading shell without bridge effects", async () => {
  const page = await import("../src/screens/project/index.tsx");
  const { SessionContext, useSession } = await import("../src/state.ts");
  function InApp() {
    return createElement(SessionContext.Provider, { value: useSession() }, createElement(page.ProjectPage, { id: "p-1", tab: "overview" }));
  }
  const markup = renderToStaticMarkup(createElement(InApp));
  assert.match(markup, /role="tablist"/);
  for (const tab of ["Overview", "Conversation", "Agents", "Changes", "Questions", "Context", "Settings"]) assert.match(markup, new RegExp(`>${tab}<`));
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
  team: { result: undefined, at: 0 }, reloadTeam: () => {},
});

test("an agent that started offers View log, a running one Stop and a waiting one Cancel", async () => {
  const agents = await import("../src/screens/project/agents.tsx");
  const running = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tabData([job("running", { started_ms: 1_000 })]) }));
  assert.match(running, /<button type="button" class="btn" aria-expanded="false" aria-controls="pj-log-1">View log<\/button>/);
  assert.match(running, /<button type="button" class="btn danger-text">Stop<\/button>/);
  assert.doesNotMatch(running, /id="pj-log-1"|>Try again<|>Cancel</);
  const queued = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tabData([job("queued")]) }));
  assert.doesNotMatch(queued, />View log<|>Stop<|>Try again</);
  assert.match(queued, /<button type="button" class="btn danger-text">Cancel<\/button>/);
});

test("research agents offer Stop while running, Cancel while waiting and Try again once stopped", () => {
  const actions = (status, extra = {}) => research.researchActions(job(status, extra));
  assert.deepEqual(actions("running", { started_ms: 1_000 }), { log: true, stop: "Stop", retry: false });
  assert.deepEqual(actions("running", { started_ms: 1_000, cancel_requested: true }), { log: true, stop: null, retry: false });
  assert.deepEqual(actions("queued"), { log: false, stop: "Cancel", retry: false });
  assert.deepEqual(actions("needs_pool"), { log: false, stop: "Cancel", retry: false });
  assert.deepEqual(actions("done", { started_ms: 1_000 }), { log: true, stop: null, retry: false });
  assert.deepEqual(actions("failed", { started_ms: 1_000 }), { log: true, stop: null, retry: true });
  assert.deepEqual(actions("cancelled"), { log: false, stop: null, retry: true });
});

test("a failed research row shows its plain reason and Try again", async () => {
  const overview = await import("../src/screens/project/overview.tsx");
  const markup = renderToStaticMarkup(createElement(overview.OverviewTab, { data: tabData([job("failed", { error: "This harness cannot run research yet", started_ms: 1_000, finished_ms: 2_000 }), job("done")]) }));
  assert.match(markup, /This harness cannot run research yet/);
  assert.equal(markup.match(/>Try again</g)?.length, 1);
});

test("a stopped or failed master turn offers its own message to send again", () => {
  const turn = (ordinal, phase) => ({ id: `t${ordinal}`, ordinal, phase, reason: null, reply: phase === "succeeded" ? { text: "hi" } : null });
  assert.equal(research.resendText([turn(1, "succeeded"), turn(2, "failed")], { t1: "first", t2: "second" }), "second");
  assert.equal(research.resendText([turn(1, "cancelled")], { t1: "first" }), "first");
  assert.equal(research.resendText([turn(2, "blocked"), turn(1, "succeeded")], { t1: "a", t2: "b" }), "b");
  assert.equal(research.resendText([turn(1, "failed"), turn(2, "succeeded")], { t1: "first", t2: "second" }), null);
  assert.equal(research.resendText([turn(1, "running")], { t1: "first" }), null);
  assert.equal(research.resendText([turn(1, "failed")], {}), null);
  assert.equal(research.resendText([turn(1, "failed")], { 1: "guessed from the ordinal" }), null);
  assert.equal(research.resendText([], {}), null);
  const crashed = { ...turn(1, "uncertain"), reason: "capture_incomplete" };
  assert.equal(research.resendText([crashed], { t1: "first" }), "first", "a turn the harness left uncertain offers Send again (LB-19)");
  assert.equal(research.turnLine(crashed), "The master stopped: its reply was cut off. (capture_incomplete)", "the line names what happened");
  assert.equal(research.replyingTurn([crashed]), null, "no Stop for a turn that already ended");
  assert.equal(research.replyingTurn([turn(1, "running")])?.id, "t1", "Stop stays for a running turn");
});

test("a failed send says Couldn't send and keeps specific engine copy", () => {
  assert.equal(research.sendFailureLine("made_up_code"), "Couldn't send. Try again. (made_up_code)");
  assert.equal(research.sendFailureLine("engine_unreachable"), `Couldn't send. ${api.failureCopy("engine_unreachable")}`);
});

test("every refusal the engine gives a master send, start or end has its own plain sentence", () => {
  const codes = ["master_source_conflict", "master_session_active", "master_session_capacity", "master_session_missing",
    "master_session_not_active", "master_turn_outstanding", "master_turn_capacity", "master_turn_not_current",
    "master_input_too_large", "master_scope_or_budget_denied", "master_pool_missing", "master_backend_account_denied",
    "rcli_profile_identity_changed"];
  for (const code of codes) assert.notEqual(research.sendFailureLine(code), "Couldn't send. Try again.", code);
});

test("only a send refused because the settings changed starts a new conversation", () => {
  const receipt = (outcome, rejection) => ({ operation_id: "op", outcome, pending: false, rejection });
  assert.equal(research.restartForSettings(receipt("rejected", "master_source_conflict")), true);
  assert.equal(research.restartForSettings(receipt("rejected", "master_turn_outstanding")), false);
  assert.equal(research.restartForSettings(receipt("applied", null)), false);
  assert.equal(research.restartForSettings({ ...receipt("rejected", "master_source_conflict"), pending: true }), false,
    "an unconfirmed send is checked, never resent");
  assert.equal(research.NEW_CONVERSATION_NOTE, "Settings changed, so a new conversation started.");
});

test("a log follows new lines only while the owner is near its bottom", () => {
  assert.equal(research.nearBottom({ scrollHeight: 1000, scrollTop: 760, clientHeight: 240 }), true);
  assert.equal(research.nearBottom({ scrollHeight: 1000, scrollTop: 700, clientHeight: 240 }), true);
  assert.equal(research.nearBottom({ scrollHeight: 1000, scrollTop: 600, clientHeight: 240 }), false);
  assert.equal(research.nearBottom({ scrollHeight: 100, scrollTop: 0, clientHeight: 240 }), true);
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
  const entry = (id, name, kind) => ({ id, name, status: { kind }, status_label: "", executable: null, headless: "", tools: { kind: "switchable" }, model_selection: false, account_kinds: [], aliases: [] });
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

test("a project change shows a toast only when the engine applied it", () => {
  const receipt = (outcome, rejection = null, pending = false) => ({ kind: "ok", value: {
    kind: "research_memory", generation: "4", project_id: "p-1", baseline_id: null, model_access: null, project_spec: null, operation_id: "o", outcome, pending, rejection,
  } });
  const promote = (result) => research.changeOutcome(result, "Now used in all projects.", "using a memory in all projects");
  assert.deepEqual(promote(receipt("applied")), { toast: "Now used in all projects.", note: "", reload: true });
  assert.deepEqual(promote(receipt("rejected", "memory_already_global")), { toast: "", note: "This memory is already in global memory.", reload: true });
  assert.deepEqual(promote(receipt("rejected", "memory_changed")), { toast: "", note: "This memory changed. Reload it and try again.", reload: true });
  assert.deepEqual(promote(receipt("uncertain", null, true)), { toast: "", note: "We couldn't confirm your last change. Check the list before trying again.", reload: true });
  assert.deepEqual(promote({ kind: "not_ready" }), { toast: "", note: "Not ready yet: using a memory in all projects isn't in this build.", reload: false });
  assert.deepEqual(promote({ kind: "error", code: "memory_missing", message: "" }), { toast: "", note: "That memory no longer exists.", reload: false });
});

test("project empty states show the brand mark, hidden from screen readers and without its name", async () => {
  const mark = /<div class="pj-empty"><span class="pj-mark large" aria-hidden="true"><span class="brand-mark large" role="img" aria-label="CrazyAgent"><svg class="brand-glyph full" width="48" height="48"/;
  const agents = await import("../src/screens/project/agents.tsx");
  const noAgents = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tabData([]) }));
  assert.match(noAgents, mark);
  assert.match(noAgents, /<h3>No agents yet<\/h3>/);
  const context = await import("../src/screens/project/context.tsx");
  const empty = { kind: "ok", value: { project_id: "p-1", folder: "/tmp/p-1", items: [] } };
  const noFiles = renderToStaticMarkup(createElement(context.ContextTab, { data: { ...tabData([]), context: empty } }));
  assert.match(noFiles, mark);
  assert.match(noFiles, /<h3>Nothing here yet<\/h3>/);
  const conversation = await import("../src/screens/project/conversation.tsx");
  const noMessages = renderToStaticMarkup(createElement(conversation.ConversationTab, { data: tabData([]) }));
  assert.doesNotMatch(noMessages, /pj-empty|brand-mark/, "the console has no empty-state card");
  assert.match(noMessages, /<p class="cs-line cs-engine"[^>]*>No messages yet\. Ask the master anything about Harbor Notes\. It has read the brief/);
});

test("the master's reply mark is the small mark with the reduced drawing for low density screens", async () => {
  const shared = await import("../src/screens/project/shared.tsx");
  const markup = renderToStaticMarkup(createElement(shared.ProjectMark));
  assert.match(markup, /^<span class="pj-mark" aria-hidden="true"><span class="brand-mark" role="img" aria-label="CrazyAgent">/);
  assert.match(markup, /<svg class="brand-glyph full" width="24" height="24" viewBox="0 0 64 64"/);
  assert.match(markup, /<svg class="brand-glyph reduced" width="24" height="24" viewBox="0 0 16 16"/);
  const css = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");
  assert.match(css, /\.pj-mark \.brand-name \{ display: none; \}/);
});

test("Needs you names the project's pool accounts that aren't signed in", () => {
  const member = (id, status) => ({ id, kind: "chatgpt", provider: "openai", status, status_label: "" });
  const pool = (members) => ({ id: "pool-1", name: "Main pool", summary: "", members, project_count: 1 });
  assert.equal(research.signInLine(pool([member("a", "signed_in"), member("b", "not_signed_in")])), "1 account in Main pool isn't signed in.");
  assert.equal(research.signInLine(pool([member("a", "not_signed_in"), member("b", "not_signed_in"), member("c", "set_up")])), "2 accounts in Main pool aren't signed in.");
  assert.equal(research.signInLine(pool([member("a", "signed_in"), member("b", "set_up")])), null);
  assert.equal(research.signInLine(undefined), null);
});

test("the Overview plugin fact counts what is on in this project, with the global switch winning", () => {
  const on = (on_here, enabled = true) => ({ on_here, enabled });
  assert.equal(research.onFact({ kind: "ok", value: [on(true), on(false), on(null), on(null, false), on(true, false)] }, "skill", "skills"), "2 skills on");
  assert.equal(research.onFact({ kind: "ok", value: [on(true)] }, "skill", "skills"), "1 skill on");
  assert.equal(research.onFact({ kind: "ok", value: [] }, "server", "servers"), "None on");
  assert.equal(research.onFact({ kind: "loading" }, "skill", "skills"), null);
  assert.equal(research.onFact({ kind: "not_ready" }, "skill", "skills"), null);
});

test("project Settings opens Plugins and skills for the old Skills and MCP servers links", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  assert.equal(settings.settingsSection("plugins"), "plugins");
  assert.equal(settings.settingsSection("skills"), "plugins");
  assert.equal(settings.settingsSection("mcp"), "plugins");
  assert.equal(settings.settingsSection("team"), "team");
  assert.equal(settings.settingsSection("nope"), "general");
  assert.equal(settings.settingsSection(null), "general");
});

test("Save pool gives the project the chosen pool or none in one step and records the choice in the project details", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  assert.deepEqual(settings.poolSteps("old", "old", "new"), { assign: true, record: true });
  assert.deepEqual(settings.poolSteps(null, null, "new"), { assign: true, record: true });
  assert.deepEqual(settings.poolSteps("new", "old", "new"), { assign: false, record: true });
  assert.deepEqual(settings.poolSteps("new", "new", "new"), { assign: false, record: false });
  assert.deepEqual(settings.poolSteps("old", undefined, "new"), { assign: true, record: false });
  assert.deepEqual(settings.poolSteps("old", "old", null), { assign: true, record: true });
  assert.deepEqual(settings.poolSteps(null, null, null), { assign: false, record: false });
});

test("Project Settings offers No pool, and a project without a pool tells the owner to pick one", async () => {
  const { PoolPicker } = await import("../src/screens/new-project/pool-picker.tsx");
  const settings = await import("../src/screens/project/settings.tsx");
  const choice = { pool: { id: "main", name: "Main", members: ["a"], projects: ["p-1"], roles: ["project_leader"], fallback_allowed: false }, members: [] };
  const markup = renderToStaticMarkup(createElement(PoolPicker, { name: "ps-pool", pools: [choice], value: null, later: true, laterLabel: settings.NO_POOL.label, laterNote: settings.NO_POOL.note, onChange: () => {} }));
  assert.match(markup, /<strong>No pool<\/strong>/);
  assert.match(markup, /The master and agents wait and show Pick a pool to start\./);
  assert.match(markup, /<label class="pj-radio-card selected"><input type="radio" name="ps-pool" checked="" value=""\/>/);
  assert.equal(settings.poolSaved(null), "This project has no pool now. Pick a pool to start.");
  assert.equal(settings.poolSaved("Main"), "Main is assigned.");
});

test("removing a project says it ends the master conversation and its folder stays on disk", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  assert.equal(settings.removeLine("/Users/me/CrazyAgent/harbor-notes"),
    "Its master conversation ends, its research and agents stop and CrazyAgent forgets it. The project folder stays on disk at ~/CrazyAgent/harbor-notes, with every file in it.");
  assert.equal(settings.removeLine(null), "Its master conversation ends, its research and agents stop and CrazyAgent forgets it. The project folder stays on disk where it is, with every file in it.");
});

test("a project set up in an existing folder shows that folder, not the .crazyagent folder inside it", async () => {
  const overview = await import("../src/screens/project/overview.tsx");
  const settings = await import("../src/screens/project/settings.tsx");
  const setup = { kind: "ok", value: {
    project_id: "p-1", name: "Harbor Notes", about: "", tech_stack: [], platforms: [], research: [],
    pool: null, team: { master: "codex", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 }, documents: [], folder: "/Users/mira/Code/harbor-notes/.crazyagent", created_ms: 0, revision: 1,
  } };
  const data = { ...tabData([]), setup };
  const pages = [
    renderToStaticMarkup(createElement(overview.OverviewTab, { data })),
    renderToStaticMarkup(createElement(settings.SettingsTab, { data, section: "general" })),
    renderToStaticMarkup(createElement(settings.SettingsTab, { data, section: "remove" })),
  ];
  for (const markup of pages) {
    assert.match(markup, /~\/Code\/harbor-notes(?!\/)/);
    assert.doesNotMatch(markup, /\.crazyagent/);
  }
  assert.equal(overview.projectFolder("C:\\Code\\harbor-notes\\.crazyagent"), "C:\\Code\\harbor-notes");
  assert.equal(overview.projectFolder("/Users/mira/CrazyAgent/projects/harbor-notes"), "/Users/mira/CrazyAgent/projects/harbor-notes");
  assert.equal(overview.projectFolder("/Users/mira/Code/.crazyagent-notes"), "/Users/mira/Code/.crazyagent-notes");
});

test("project Settings, General edits the saved details with the New Project controls", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  const setup = { kind: "ok", value: {
    project_id: "p-1", name: "Harbor Notes", about: "Notes for marine engineers.", tech_stack: ["Rust"], platforms: ["desktop_app"], research: ["market_research"],
    pool: null, team: { master: "codex", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 }, documents: [], folder: "/tmp/harbor-notes", created_ms: 0, revision: 2,
  } };
  const markup = renderToStaticMarkup(createElement(settings.SettingsTab, { data: { ...tabData([]), setup }, section: "general" }));
  assert.match(markup, /<label for="ps-name">Project name<\/label><input class="pj-input" id="ps-name"[^>]*value="Harbor Notes"/);
  assert.match(markup, /<label for="ps-about">What is it about\?<\/label>/);
  assert.match(markup, /<legend>Tech stack<\/legend>/);
  assert.match(markup, /<legend>Target platforms<\/legend>/);
  assert.match(markup, /<legend>Research<\/legend>/);
  assert.match(markup, /id="ps-research-market_research"/);
  assert.match(markup, /<button[^>]*type="submit"[^>]*disabled=""[^>]*>.*Save changes/);
  assert.doesNotMatch(markup, /isn&#x27;t in this build/);
  assert.doesNotMatch(markup, /role="alert"/);
});

test("project Settings never seeds its forms from another project's setup, so a save can't write it into this one", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  const other = { kind: "ok", value: {
    project_id: "p-2", name: "Bravo Notes", about: "Bravo's own details.", tech_stack: ["Go"], platforms: ["web_app"], research: ["competitor_research"],
    pool: null, team: { master: "claude", master_tools: false, sub_harnesses: ["codex"], min_agents: 2, max_agents: 3 }, documents: [], folder: "/tmp/bravo-notes", created_ms: 0, revision: 9,
  } };
  for (const section of ["general", "team"]) {
    const markup = renderToStaticMarkup(createElement(settings.SettingsTab, { data: { ...tabData([]), setup: other }, section }));
    assert.match(markup, /role="status">Loading\.\.\.</, section);
    assert.doesNotMatch(markup, /Bravo|<form|Save changes|Save team/, section);
  }
  const own = { kind: "ok", value: { ...other.value, project_id: "p-1", name: "Harbor Notes" } };
  const general = renderToStaticMarkup(createElement(settings.SettingsTab, { data: { ...tabData([]), setup: own }, section: "general" }));
  assert.match(general, /id="ps-name"[^>]*value="Harbor Notes"/);
});

test("selected report text is cleaned before it can be saved to memory", async () => {
  const context = await import("../src/screens/project/context.tsx");
  assert.equal(context.selectionText("  Owners pay per seat.  "), "Owners pay per seat.");
  assert.equal(context.selectionText("line one\r\nline two"), "line one\nline two");
  assert.equal(context.selectionText(" \n\t "), null);
  assert.equal(context.selectionText(""), null);
});

test("a selection keeps hidden and reordering characters visible, so the saved note is what the owner saw", async () => {
  const context = await import("../src/screens/project/context.tsx");
  assert.equal(context.selectionText("Use pnpm\u{e0041}\u{e0042} for installs\u200b."), "Use pnpm\\u{e0041}\\u{e0042} for installs\\u200b.");
  assert.equal(context.selectionText("Rename\u202etxt.exe"), "Rename\\u202etxt.exe");
  assert.equal(context.selectionText("\u200b"), "\\u200b");
  assert.equal(context.selectionText("Warn with ⚠\ufe0f"), "Warn with ⚠\ufe0f");
});

test("a memory note must have text and fit the engine's 2,000 byte limit", async () => {
  const context = await import("../src/screens/project/context.tsx");
  assert.equal(context.noteError("Owners pay per seat."), null);
  assert.equal(context.noteError("  \n "), "Write what agents should remember.");
  assert.equal(context.noteError("a".repeat(2000)), null);
  assert.equal(context.noteError(`  ${"a".repeat(2000)}  `), null);
  assert.equal(context.noteError("a".repeat(2001)), "This is too long for one memory. Keep it under 2,000 characters.");
  assert.equal(context.noteError("é".repeat(1001)), "This is too long for one memory. Keep it under 2,000 characters.");
});

const teamAgent = (id, status, extra = {}) => ({ id, role: "sub", label: extra.role === "master" ? "Master" : `Sub-agent ${id}`, harness: "codex", tools: true, account: null, worktree: null, status, status_label: "", task: null, turns: 0, problem: null, process_group: null, created_ms: 0, heartbeat_ms: 0, stopped_ms: null, ...extra });
const teamTask = (id, status, extra = {}) => ({ id, source: "master", harness: "codex", tools: true, prompt: `Task ${id}`, status, status_label: "", agent: null, attempts: 1, result: null, problem: null, delivered: false, output_bytes: 0, created_ms: 0, started_ms: null, finished_ms: null, ...extra });

test("team agents and tasks use the design's status words", () => {
  assert.deepEqual(["starting", "idle", "working", "stopped", "failed"].map(research.teamAgentWord), ["Starting", "Ready", "Running", "Stopped", "Failed"]);
  assert.deepEqual(["queued", "needs_pool", "running", "done", "failed", "cancelled"].map(research.teamTaskWord), ["Queued", "Needs a pool", "Running", "Done", "Failed", "Stopped"]);
});

test("the Team panel names each agent, its account and current task, and lists tasks from master to sub-agent", () => {
  const status = {
    agents: [
      teamAgent(1, "working", { role: "master", harness: "claude", account: "acct-a" }),
      teamAgent(2, "working", { task: 10, turns: 3, account: "acct-b", worktree: "worktrees/agent-2" }),
      teamAgent(3, "idle", { account: "acct-gone" }),
      teamAgent(4, "failed", { problem: "Codex stopped answering." }),
    ],
    tasks: [
      teamTask(10, "running", { agent: 2, prompt: "Build the login screen\nwith tests", started_ms: 1_000 }),
      teamTask(11, "queued", { source: "owner", prompt: "Write the README" }),
      teamTask(12, "done", { agent: 3, result: "## Summary\n\nLogin screen built, 4 tests pass.", started_ms: 0, finished_ms: 120_000 }),
      teamTask(13, "failed", { problem: "No account in this project's pool can run codex" }),
    ],
  };
  const accounts = [{ id: "acct-a", name: "Work Claude" }, { id: "acct-b", name: "Codex main" }];
  const label = (id) => ({ codex: "Codex", claude: "Claude Code" })[id] ?? id;
  const running = research.teamPanel(status, accounts, label, "running", 61_000);
  assert.deepEqual(running.agents, [
    { key: "agent-1", role: "Master", name: "Master", harness: "Claude Code", account: "Work Claude", word: "Running", task: "", folder: "Works in the project folder", rename: null,
      depth: 0, lead: null, stop: null, stoppedByYou: false, teammates: [], messageNote: null },
    { key: "agent-2", role: "Sub-agent", name: "Sub-agent 2", harness: "Codex", account: "Codex main", word: "Running", task: "Build the login screen", folder: "Works in worktrees/agent-2", rename: null,
      depth: 0, lead: null, stop: { agent: 2 }, stoppedByYou: false, teammates: [], messageNote: null },
    { key: "agent-3", role: "Sub-agent", name: "Sub-agent 3", harness: "Codex", account: "An account in the pool", word: "Ready", task: "Last task: Task 12", folder: "", rename: null,
      depth: 0, lead: null, stop: { agent: 3 }, stoppedByYou: false, teammates: [], messageNote: null },
  ], "the master has no Stop; each live sub-agent has its own");
  assert.deepEqual(running.tasks.map((row) => [row.key, row.from, row.to, row.word, row.prompt, row.summary, row.elapsed, row.stop]), [
    ["task-10", "From the master", "Sub-agent 2", "Running", "Build the login screen", "", "1 min", { task: 10, label: "Stop" }],
    ["task-11", "From you", "Codex", "Queued", "Write the README", "", "", { task: 11, label: "Cancel" }],
  ]);
  const all = research.teamPanel(status, accounts, label, "all", 61_000);
  assert.deepEqual(all.agents[3], { key: "agent-4", role: "Sub-agent", name: "Sub-agent 4", harness: "Codex", account: "No account yet", word: "Failed", task: "Codex stopped answering.", folder: "", rename: null,
    depth: 0, lead: null, stop: null, stoppedByYou: false, teammates: [], messageNote: null }, "a failed sub-agent has nothing left to stop");
  assert.deepEqual(all.tasks.map((row) => [row.key, row.to, row.word, row.summary, row.elapsed, row.stop]), [
    ["task-10", "Sub-agent 2", "Running", "", "1 min", { task: 10, label: "Stop" }],
    ["task-11", "Codex", "Queued", "", "", { task: 11, label: "Cancel" }],
    ["task-12", "Sub-agent 3", "Done", "Login screen built, 4 tests pass.", "2 min", null],
    ["task-13", "Codex", "Failed", "No account in this project's pool can run codex", "", null],
  ]);
  assert.equal(all.tasks[2].result, "## Summary\n\nLogin screen built, 4 tests pass.");
  assert.deepEqual(research.teamPanel({ agents: [], tasks: [] }, [], label, "all", 0), { agents: [], tasks: [] });
  const named = research.teamPanel({
    agents: [teamAgent(5, "working", { task: 20, label: "Ada (@onboarding-fix)", persona: "Ada", handle: "onboarding-fix" }), teamAgent(6, "idle")],
    tasks: [teamTask(20, "running", { agent: 5, prompt: "Fix onboarding", started_ms: 0 })],
  }, accounts, label, "all", 0);
  assert.deepEqual(named.agents.map((row) => [row.name, row.harness, row.rename]), [["Ada (@onboarding-fix)", "Codex", { agent: 5, handle: "onboarding-fix" }], ["Sub-agent 6", "Codex", null]]);
  const stopped = research.teamPanel({ agents: [teamAgent(7, "stopped", { label: "Grace (@docs)", persona: "Grace", handle: "docs" })], tasks: [] }, [], label, "all", 0);
  assert.equal(stopped.agents[0].rename, null);
  const led = research.teamPanel({
    agents: [teamAgent(8, "idle", { label: "Grace (@lead-b)" }), teamAgent(9, "working", { task: 21, parent: 8 })],
    tasks: [teamTask(21, "running", { agent: 9, parent: 8 }), teamTask(22, "queued", { parent: 99 }), teamTask(23, "queued", { source: "owner" }), teamTask(24, "queued", { parent: null })],
  }, accounts, label, "all", 0);
  assert.deepEqual(led.tasks.map((row) => row.from), ["From Grace (@lead-b)", "From a lead", "From you", "From the master"],
    "a task a lead handed out names that lead");
  assert.deepEqual(led.agents.map((row) => [row.name, row.depth, row.lead]), [["Grace (@lead-b)", 0, null], ["Sub-agent 9", 1, "Grace (@lead-b)"]],
    "a worker sits one level under the lead that started it and names it");
  assert.equal(named.tasks[0].to, "Ada (@onboarding-fix)");
  assert.deepEqual(led.agents.map((row) => [row.key, row.depth, row.lead]), [["agent-8", 0, null], ["agent-9", 1, "Grace (@lead-b)"]]);
});

test("the Team panel lists each worker under its lead, and offers Stop only on a live sub-agent or teammate the owner did not stop", () => {
  const status = {
    agents: [
      teamAgent(1, "working", { role: "master" }),
      teamAgent(2, "working", { label: "Lin (@search)" }),
      teamAgent(3, "working", { label: "Ada (@notes)", teammates: [{ pane: "%4", ended: false, stopped: false }, { pane: "%5", ended: true, stopped: false }, { pane: "%6", ended: false, stopped: true }] }),
      teamAgent(4, "working", { parent: 3 }),
      teamAgent(5, "idle", { parent: 2 }),
      teamAgent(6, "working", { parent: 4 }),
      teamAgent(7, "stopped", { parent: 3, owner_stopped: true }),
      teamAgent(8, "working", { owner_stopped: true, teammates: [{ pane: "%9", ended: false, stopped: false }] }),
    ],
    tasks: [],
  };
  const rows = research.teamPanel(status, [], (id) => id, "all", 0).agents;
  assert.deepEqual(rows.map((row) => [row.key, row.depth, row.lead]), [
    ["agent-1", 0, null],
    ["agent-2", 0, null],
    ["agent-5", 1, "Lin (@search)"],
    ["agent-3", 0, null],
    ["agent-4", 1, "Ada (@notes)"],
    ["agent-6", 2, "Sub-agent 4"],
    ["agent-7", 1, "Ada (@notes)"],
    ["agent-8", 0, null],
  ]);
  const byKey = new Map(rows.map((row) => [row.key, row]));
  assert.equal(byKey.get("agent-1").stop, null, "the master stops with Stop all");
  assert.deepEqual(byKey.get("agent-6").stop, { agent: 6 }, "a worker of a worker can be stopped");
  assert.deepEqual([byKey.get("agent-7").stop, byKey.get("agent-7").stoppedByYou], [null, true]);
  assert.deepEqual([byKey.get("agent-8").stop, byKey.get("agent-8").stoppedByYou], [null, true]);
  assert.deepEqual(byKey.get("agent-3").teammates, [
    { key: "teammate-3-%4", pane: "%4", word: "Running", stop: { agent: 3, pane: "%4" } },
    { key: "teammate-3-%5", pane: "%5", word: "Closed", stop: null },
    { key: "teammate-3-%6", pane: "%6", word: "Stopped by you", stop: null },
  ]);
  assert.deepEqual(byKey.get("agent-8").teammates.map((row) => [row.word, row.stop]), [["Running", null]], "a harness the owner stopped offers no teammate Stop");
  assert.deepEqual(byKey.get("agent-4").teammates, []);
  assert.deepEqual(research.leadTree([{ id: 1, parent: 2 }, { id: 2, parent: 1 }, { id: 3, parent: 3 }]).map(({ agent, depth }) => [agent.id, depth]), [[3, 0], [1, 0], [2, 0]],
    "agents that name each other as lead are each listed once");
});

test("a task result summary is its first plain line", () => {
  assert.equal(research.resultSummary("## Summary\n\nLogin built."), "Login built.");
  assert.equal(research.resultSummary("- first point\n- second"), "first point");
  assert.equal(research.resultSummary("> quoted answer"), "quoted answer");
  assert.equal(research.resultSummary("# Only a heading"), "Only a heading");
  assert.equal(research.resultSummary(""), "");
  assert.equal(research.resultSummary("x".repeat(300)), `${"x".repeat(199)}...`);
});

test("a long task prompt shows its first line, cut to fit a row", () => {
  assert.equal(research.firstLine("  one\ntwo"), "one");
  assert.equal(research.firstLine("x".repeat(200)), `${"x".repeat(139)}...`);
  assert.equal(research.firstLine(""), "");
});

test("the Agents tab counts working team agents and running research like the project card", async () => {
  const agents = await import("../src/screens/project/agents.tsx");
  const team = { project_id: "p-1", keep_warm: false, paused: false, min_agents: 0, max_agents: 5, harnesses: [], messages: [], plan: [],
    agents: [teamAgent(2, "working", { task: 10 }), teamAgent(3, "working", { task: 11 }), teamAgent(4, "stopped")],
    tasks: [teamTask(10, "running", { agent: 2 }), teamTask(11, "running", { agent: 3 })],
    events: { recent: [], waiting: [], notifications: [], paused: false, questions: [],
      wake: { app: null, app_version: 0, project: null, project_version: 0, agents: [], cleared: [], effective: { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 }, level: "shipped" } } };
  const data = { ...tabData([job("running", { started_ms: 1_000 })]), setup: { kind: "ok", value: { team: { max_agents: 5 } } },
    assigned: { pool: { id: "pool-1", name: "Work", members: ["a", "b"] } }, team: { result: { kind: "ok", value: team }, at: 1_000 } };
  const markup = renderToStaticMarkup(createElement(agents.AgentsTab, { data }));
  assert.match(markup, /3 agents working\. Work: 2 accounts\./);
  assert.doesNotMatch(markup, /1 of 5 agents running/);
});

test("the Agents tab labels research and asks the engine for the live team", async () => {
  const agents = await import("../src/screens/project/agents.tsx");
  const markup = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tabData([job("running", { started_ms: 1_000 })]) }));
  assert.match(markup, /<h3>Research<\/h3><ul class="pj-rows" aria-label="Research agents">/);
  assert.match(markup, /Loading the team\.\.\./);
});

test("a team agent keeps the engine's name in every row, whatever its harness or place", () => {
  const status = {
    agents: [
      teamAgent(1, "working", { role: "master", harness: "claude", label: "Master" }),
      teamAgent(2, "idle", { label: "Sub-agent 2" }),
      teamAgent(3, "working", { harness: "claude", task: 10, label: "Sub-agent 3" }),
      teamAgent(4, "stopped", { label: "Sub-agent 4" }),
    ],
    tasks: [
      teamTask(10, "running", { harness: "claude", agent: 3 }),
      teamTask(11, "done", { agent: 4 }),
    ],
  };
  const label = (id) => ({ codex: "Codex", claude: "Claude Code" })[id] ?? id;
  const all = research.teamPanel(status, [], label, "all", 0);
  assert.deepEqual(all.agents.map((row) => [row.name, row.harness]),
    [["Master", "Claude Code"], ["Sub-agent 2", "Codex"], ["Sub-agent 3", "Claude Code"], ["Sub-agent 4", "Codex"]]);
  assert.deepEqual(all.tasks.map((row) => row.to), ["Sub-agent 3", "Sub-agent 4"]);
});

test("every team agent row shows its current task or, between tasks, its last one", () => {
  const status = {
    agents: [teamAgent(2, "working", { task: 10 }), teamAgent(3, "idle"), teamAgent(4, "stopped"), teamAgent(5, "idle")],
    tasks: [
      teamTask(9, "done", { agent: 4, prompt: "Old work" }),
      teamTask(10, "running", { agent: 2, prompt: "Build the login screen" }),
      teamTask(11, "done", { agent: 4, prompt: "Fix the header\nand the footer" }),
      teamTask(12, "failed", { agent: 3, prompt: "Write the tests" }),
    ],
  };
  assert.deepEqual(research.teamPanel(status, [], (id) => id, "all", 0).agents.map((row) => row.task),
    ["Build the login screen", "Last task: Write the tests", "Last task: Fix the header", "Waiting for work"]);
});

test("team tasks waiting for a pool ask the owner for one, with the engine's reason", () => {
  assert.equal(research.teamWaitingLine([teamTask(1, "running"), teamTask(2, "done")]), null);
  assert.equal(research.teamWaitingLine([teamTask(1, "needs_pool", { problem: "No account in this project's pool can run codex" })]),
    "A team task waits: No account in this project's pool can run codex.");
  assert.equal(research.teamWaitingLine([teamTask(1, "needs_pool"), teamTask(2, "needs_pool", { problem: "No account in this project's pool can run codex." })]),
    "2 team tasks wait: No account in this project's pool can run codex.");
  assert.equal(research.teamWaitingLine([teamTask(1, "needs_pool")]), "A team task waits for an account in this project's pool.");
  assert.equal(research.teamWaitingLine([]), null);
});

test("Pause all agents says what stops, or that nothing is running", () => {
  assert.deepEqual(research.pauseSummary([teamTask(1, "running"), teamTask(2, "running"), teamTask(3, "queued"), teamTask(4, "needs_pool"), teamTask(5, "done")]),
    { count: 4, line: "2 running tasks stop and 2 waiting tasks are cancelled." });
  assert.deepEqual(research.pauseSummary([teamTask(1, "running")]), { count: 1, line: "1 running task stops." });
  assert.deepEqual(research.pauseSummary([teamTask(1, "queued")]), { count: 1, line: "1 waiting task is cancelled." });
  assert.deepEqual(research.pauseSummary([teamTask(1, "done"), teamTask(2, "cancelled")]), { count: 0, line: "No team agents are working or waiting." });
  assert.deepEqual(research.pauseSummary([]), { count: 0, line: "No team agents are working or waiting." });
});

test("Project Settings sends the engine's team fields only, never the tools choice kept while a harness forces tools on", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  const stored = { about: "a", tech_stack: ["Rust"], platforms: ["cli"], research: [], pool: "main" };
  const team = {
    master: "cursor", master_tools: true, master_tools_wanted: false,
    sub_harnesses: [{ harness: "cursor", agents: 2, tools: true, tools_wanted: false }], min_agents: 1, max_agents: 3,
  };
  assert.deepEqual(settings.setupOf(stored, team), {
    about: "a", tech_stack: ["Rust"], platforms: ["cli"], research: [], pool: "main", documents: [],
    team: { master: "cursor", master_tools: true, sub_harnesses: [{ harness: "cursor", agents: 2, tools: true }], min_agents: 1, max_agents: 3 },
  });
  assert.deepEqual(settings.setupOf({ ...stored, research: ["market_research", "roadmap_planning"] }, team).research, ["market_research", "roadmap_planning"],
    "saving the team keeps the stored roadmap choice");
});

test("the master ticket is used only for the project that was opened", () => {
  const opened = { project_id: "alpha", ticket: "7", brief: null, fresh: true, revision: "1" };
  assert.equal(research.openedTicket(opened, "alpha"), "7");
  assert.throws(() => research.openedTicket(opened, "beta"), (failure) => failure.code === "master_project_mismatch");
});

test("a master opened for another project has its own plain sentence", () => {
  assert.equal(api.failureCopy("master_project_mismatch"), "The conversation opened for another project. Open this project again.");
});

test("files named like object built-ins get the plain file icon instead of crashing the tree", () => {
  for (const path of ["context/documents/a.constructor", "context/research/b.__PROTO__", "context/documents/c.hasownproperty", "context/documents/d.tostring"]) {
    assert.equal(research.fileIcon(path), "file", path);
  }
});

test("an engine code named like an object built-in gets the plain fallback sentence", () => {
  for (const code of ["constructor", "__proto__", "toString"]) {
    assert.equal(typeof api.failureCopy(code), "string", code);
    assert.equal(api.failureCopy(code), `Something went wrong. Try again. (${/^[a-z0-9_]+$/.test(code) ? code : "unknown"})`, code);
  }
});

test("a context read that answers after the reader moved on is dropped", async () => {
  const context = await import("../src/screens/project/context.tsx");
  const settle = () => new Promise((resolve) => setTimeout(resolve, 50));
  const late = [];
  const stop = context.followContext("alpha", "context/brief.md", false, (result) => late.push(result));
  stop();
  await settle();
  assert.deepEqual(late, []);
  const kept = [];
  context.followContext("alpha", "context/brief.md", false, (result) => kept.push(result));
  await settle();
  assert.equal(kept.length, 1);
  assert.equal(kept[0].kind, "error");
});

test("a failed add offers a single picked path to retry and never several joined into one", () => {
  assert.equal(research.retryPath(["/Users/mira/a.pdf"]), "/Users/mira/a.pdf");
  assert.equal(research.retryPath(["/Users/mira/a.pdf", "/Users/mira/b.pdf"]), "");
  assert.equal(research.retryPath([]), "");
});

test("Enter and the Send button follow one rule", () => {
  assert.equal(research.MESSAGE_BYTES_MAX, 8192);
  assert.equal(research.canSend("hello", false), true);
  assert.equal(research.canSend("hello", true), false);
  assert.equal(research.canSend("   ", false), false);
  assert.equal(research.canSend("a".repeat(8192), false), true);
  assert.equal(research.canSend("a".repeat(8193), false), false);
  assert.equal(research.canSend("é".repeat(4097), false), false);
});

test("a turn the engine asked for shows the engine's line where the owner's message would be, as plain text", async () => {
  const conversation = await import("../src/screens/project/conversation.tsx");
  const line = "CrazyAgent asked the master to resolve the conflicts in README.md.";
  const asked = { id: "t1", ordinal: 1, phase: "running", reason: null, reply: null, engine_request: { change_set: "c".repeat(32), line, text: "Resolve the conflicts." } };
  assert.equal(research.engineLine(asked), line);
  assert.equal(research.engineLine({ ...asked, engine_request: null }), null);
  assert.equal(research.engineLine({ id: "t2", ordinal: 2, phase: "succeeded", reason: null, reply: { text: "Done." } }), null);
  const opening = (turn, sent) => conversation.consoleLines([turn], sent === undefined ? {} : { [turn.id]: sent }, {}, 0)[0];
  assert.deepEqual(opening(asked), { id: "t1-owner", kind: "engine", text: line });
  assert.deepEqual(opening({ ...asked, engine_request: undefined }), { id: "t1-owner", kind: "engine", text: "Your message 1 (its text stays in the window that sent it)" });
  assert.deepEqual(opening(asked, "Fix it"), { id: "t1-owner", kind: "owner", text: "Fix it" });
  const { Console } = await import("../src/screens/project/console.tsx");
  const draw = (turn) => renderToStaticMarkup(createElement(Console, { projectId: "p-1", lines: conversation.consoleLines([turn], {}, {}, 0), agents: [], onSend: () => {}, onStop: () => {}, onNavigate: () => {} }));
  const html = draw(asked);
  assert.ok(html.includes(line.replace("'", "&#x27;")), html);
  assert.doesNotMatch(html, /Resolve the conflicts\./);
  const tricky = draw({ ...asked, engine_request: { ...asked.engine_request, line: "<b>x</b>\u001b[31m" } });
  assert.ok(tricky.includes("&lt;b&gt;x&lt;/b&gt;"), tricky);
  assert.doesNotMatch(tricky, /<b>x<\/b>/);
});

test("a master turn whose edits wait in Changes shows the engine's note, and a turn without one shows nothing", () => {
  const waiting = "The edits from this turn are waiting for you in Changes.";
  assert.equal(research.changesLine({ id: "t1", ordinal: 1, phase: "succeeded", reason: null, reply: { text: "Done." }, changes: waiting }), waiting);
  assert.equal(research.changesLine({ id: "t2", ordinal: 2, phase: "failed", reason: null, reply: null, changes: null }), null);
  assert.equal(research.changesLine({ id: "t3", ordinal: 3, phase: "succeeded", reason: null, reply: { text: "Done." } }), null);
  assert.equal(research.changesLine({ id: "t4", ordinal: 4, phase: "succeeded", reason: null, reply: { text: "Done." }, changes: "  " }), null);
});

const ideView = (build, platform = "macos", unqualified = null) => ({ state: { build, unqualified }, platform });

test("the IDE tab replaces Context and Changes only on a qualified platform with a Ready build", async () => {
  const page = await import("../src/screens/project/index.tsx");
  const client = await import("../src/api-ide.ts");
  const ids = (view) => page.projectTabs(view).map((tab) => tab.id);
  const fixed = ["overview", "conversation", "agents", "changes", "questions", "context", "process", "metrics", "settings"];
  const swapped = ["overview", "conversation", "agents", "ide", "questions", "process", "metrics", "settings"];
  assert.deepEqual(ids(undefined), fixed, "until the engine answers, the page keeps Context and Changes and names no IDE");
  assert.equal(client.ideUnavailable(undefined), null);
  const ready = ideView({ state: "ready", version: "1.143.17183", root: "/data/ide/build" });
  assert.deepEqual(ids(ready), swapped);
  assert.equal(page.projectTabs(ready).find((tab) => tab.id === "ide").label, "IDE");
  assert.equal(client.ideUnavailable(ready), null);
  const fetching = [
    [ideView({ state: "downloading", bytes: 48 * 1024 * 1024, total: 96 * 1024 * 1024 }), "Downloading the IDE: 48 of 96 MB."],
    [ideView({ state: "verifying" }), "Checking the IDE download."],
  ];
  for (const [view, line] of fetching) {
    assert.deepEqual(ids(view), fixed, `${line} Changes and Context stay usable until the IDE can open`);
    assert.deepEqual(client.ideUnavailable(view), { line, action: null }, "the fetch shows its progress, not an unavailable platform");
  }
  const unavailable = [
    [ideView({ state: "ready", version: "1.143.17183", root: "/x" }, "linux", "linux"),
      "IDE unavailable on Linux: The IDE isn't supported on this platform yet.", null],
    [ideView({ state: "downloading", bytes: 1, total: 2 }, "windows", "windows"),
      "IDE unavailable on Windows: The IDE isn't supported on this platform yet.", null],
    [ideView({ state: "missing" }, "windows", "windows"),
      "IDE unavailable on Windows: The IDE isn't supported on this platform yet.", null],
    [ideView({ state: "failed", code: "ide_build_hash_mismatch" }),
      "IDE unavailable on macOS: The IDE download didn't match its published checksum, so it was deleted and never run.", "retry"],
    [ideView({ state: "failed", code: "ide_build_unsupported_platform" }),
      "IDE unavailable on macOS: This CrazyAgent version has no IDE build for this platform.", null],
    [ideView({ state: "missing" }),
      "IDE unavailable on macOS: The IDE isn't downloaded yet.", "download"],
  ];
  for (const [view, line, action] of unavailable) {
    assert.deepEqual(ids(view), fixed, line);
    assert.deepEqual(client.ideUnavailable(view), { line, action }, line);
  }
});

test("while the build downloads or verifies, the page reads the IDE state every second until it is Ready", async () => {
  const client = await import("../src/api-ide.ts");
  assert.equal(client.ideFetching(undefined), false);
  assert.equal(client.ideFetching(ideView({ state: "downloading", bytes: 1, total: 2 })), true);
  assert.equal(client.ideFetching(ideView({ state: "verifying" })), true);
  assert.equal(client.ideFetching(ideView({ state: "downloading", bytes: 1, total: 2 }, "linux", "linux")), false, "an unqualified platform never opens, so nothing to wait for");
  for (const build of [{ state: "missing" }, { state: "ready", version: "1", root: "/x" }, { state: "failed", code: "ide_build_storage" }]) {
    assert.equal(client.ideFetching(ideView(build)), false, build.state);
  }
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /const fetching = ideFetching\(view\);/);
  assert.match(index, /fetching \? window\.setInterval\(read, IDE_POLL_MS\) : 0/);
  assert.match(index, /\}, \[projectId, session\?\.revision, fetching\]\);/, "the poll starts once a download starts and stops at Ready or Failed");
  const tab = readFileSync(new URL("../src/screens/project/ide.tsx", import.meta.url), "utf8");
  assert.match(tab, /export const IDE_POLL_MS = 1_000;/);
});

test("routes to a replaced tab keep their target, listed beside the IDE while open, and the IDE route lands on Changes where it can't open", async () => {
  const page = await import("../src/screens/project/index.tsx");
  const ready = ideView({ state: "ready", version: "1.143.17183", root: "/x" });
  const failed = ideView({ state: "failed", code: "ide_build_download_failed" });
  const ids = (view, route) => page.projectTabs(view, route).map((tab) => tab.id);
  const routed = (route, view) => page.routedTab(route, page.projectTabs(view, route));
  assert.deepEqual(ids(ready, "overview"), ["overview", "conversation", "agents", "ide", "questions", "process", "metrics", "settings"]);
  assert.deepEqual(ids(ready, "changes"), ["overview", "conversation", "agents", "ide", "changes", "questions", "process", "metrics", "settings"], "an open Changes is listed after the IDE");
  assert.deepEqual(ids(ready, "context"), ["overview", "conversation", "agents", "ide", "questions", "context", "process", "metrics", "settings"]);
  assert.equal(routed("changes", ready), "changes");
  assert.equal(routed("context", ready), "context", "Overview's Review and Open report links open their file in Context");
  assert.equal(routed("ide", ready), "ide");
  assert.equal(routed("process", ready), "process");
  assert.equal(routed("ide", failed), "changes");
  assert.equal(routed("context", failed), "context");
  assert.equal(routed("nowhere", ready), "overview");
  assert.equal(routed("nowhere", failed), "overview");
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /const tabs = projectTabs\(view, tab\);/);
  assert.match(index, /tabs\.map\(\(item\) => item\.id\)\.join\(" "\)\]\);/, "the number shortcuts follow the listed tabs, not only their count");
});

test("every Changes and Context action stays one menu choice away while the IDE replaces them", async () => {
  const page = await import("../src/screens/project/index.tsx");
  const ids = (view) => page.replacedTabs(view).map((tab) => tab.id);
  assert.deepEqual(ids(ideView({ state: "ready", version: "1.143.17183", root: "/x" })), ["changes", "context"]);
  assert.deepEqual(ids(ideView({ state: "downloading", bytes: 1, total: 2 })), []);
  assert.deepEqual(ids(ideView({ state: "failed", code: "ide_build_storage" })), []);
  assert.deepEqual(ids(undefined), []);
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  const menu = index.slice(index.indexOf("function ProjectMenu"), index.indexOf("function hashSection"));
  assert.match(menu, /replaced\.map\(\(item\) => \(\{ label: `Open \$\{item\.label\}`/, "the project menu offers Open Changes and Open Context");
  assert.match(menu, /data\.navigate\(`\/projects\/\$\{encodeURIComponent\(data\.id\)\}\/\$\{item\.id\}`\)/);
  assert.match(index, /<ProjectMenu data=\{data\} replaced=\{replacedTabs\(view\)\} \/>/);
});

test("a failed IDE state read shows the failure with Retry instead of falling back silently", async () => {
  const client = await import("../src/api-ide.ts");
  const ide = await import("../src/screens/project/ide.tsx");
  const refused = { kind: "error", code: "ide_tab_outcome_unknown", message: "raw" };
  assert.deepEqual(client.ideReadFailed(refused, "macos"), { line: `IDE unavailable on macOS: ${api.failureCopy(refused)}`, action: "retry" });
  assert.deepEqual(client.ideReadFailed({ kind: "not_ready" }, "linux"),
    { line: "IDE unavailable on Linux: CrazyAgent's engine isn't ready yet. Try again in a moment.", action: "retry" });
  assert.equal(client.desktopPlatform("MacIntel"), "macos");
  assert.equal(client.desktopPlatform("Win32"), "windows");
  assert.equal(client.desktopPlatform("Linux x86_64"), "linux");
  assert.equal(client.desktopPlatform("FreeBSD amd64"), "FreeBSD amd64");
  const markup = renderToStaticMarkup(createElement(ide.IdeUnavailable, { view: undefined, failure: { kind: "not_ready" }, onAction: () => {} }));
  assert.match(markup, /role="status"/);
  assert.ok(markup.includes("IDE unavailable on ") && markup.includes("CrazyAgent&#x27;s engine isn&#x27;t ready yet."), markup);
  assert.match(markup, />Retry</);
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /useState<Result<IdeView>>\(\)/, "the page keeps a failed read, not only a good one");
  assert.match(index, /failure=\{failure\}/);
  assert.match(index, /failure \? readIde\(\) : fetchIde\(\)/, "Retry after a failed read reads again and starts no download");
});

test("the unavailable line names the platform and reason and offers the build's one action", async () => {
  const ide = await import("../src/screens/project/ide.tsx");
  const draw = (view) => renderToStaticMarkup(createElement(ide.IdeUnavailable, { view, onAction: () => {} }));
  const missing = draw(ideView({ state: "missing" }));
  assert.match(missing, /role="status"/);
  assert.ok(missing.includes("IDE unavailable on macOS: The IDE isn&#x27;t downloaded yet."), missing);
  assert.match(missing, />Download</);
  const failed = draw(ideView({ state: "failed", code: "ide_build_storage" }));
  assert.match(failed, />Retry</);
  const linux = draw(ideView({ state: "ready", version: "1.143.17183", root: "/x" }, "linux", "linux"));
  assert.ok(linux.includes("IDE unavailable on Linux: The IDE isn&#x27;t supported on this platform yet."), linux);
  assert.doesNotMatch(linux, /<button/);
  assert.equal(draw(ideView({ state: "ready", version: "1.143.17183", root: "/x" })), "", "a shown IDE needs no line");
  const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(index, /<IdeUnavailable /, "the project page shows the line above its tabs");
  assert.match(index, /onView=\{/, "the IDE tab reports each state it reads, so a failed download brings Context and Changes back");
});

test("the ide_tab flag is gone: the desktop always asks the engine and names its own platform", () => {
  const bridge = readFileSync(new URL("../src-tauri/src/bridge/ide.rs", import.meta.url), "utf8");
  assert.doesNotMatch(bridge, /CRAZYAGENT_IDE_TAB|IDE_TAB_FLAG|ide_tab_on|IdeView::Off/);
  assert.match(bridge, /platform: std::env::consts::OS/);
  const client = readFileSync(new URL("../src/api-ide.ts", import.meta.url), "utf8");
  assert.doesNotMatch(client, /ide_tab_off|tab: "off"/);
});

test("every desktop memory list and count reads all memory batches, never one engine batch of 8", async () => {
  const shared = await import("../src/api.ts");
  assert.equal(typeof shared.loadAllProjectMemories, "function");
  const root = new URL("../src/", import.meta.url);
  const source = (path) => readFileSync(new URL(path, root), "utf8");
  const sources = (dir) => readdirSync(new URL(dir, root), { withFileTypes: true }).flatMap((entry) => entry.isDirectory()
    ? sources(`${dir}${entry.name}/`) : /\.tsx?$/.test(entry.name) ? [`${dir}${entry.name}`] : []);
  const screens = sources("").filter((path) => path !== "engine-client.ts");
  const readers = screens.filter((path) => source(path).includes('"desktop_project_memories"'));
  assert.deepEqual(readers, ["api.ts"], "one engine batch holds at most 8 entries of any status, so only the paging loader reads it");
  assert.deepEqual(screens.filter((path) => /\bprojectMemories\(/.test(source(path))), [], "nothing reads one batch through the typed client");
  const body = (text, name) => text.match(new RegExp(`export async function ${name}[\\s\\S]*?\\n}\\n`))?.[0] ?? "";
  const all = body(source("api.ts"), "loadAllProjectMemories");
  assert.match(all, /page < MEMORY_PAGES_MAX/);
  assert.match(all, /"desktop_project_memories", \{ id, after \}/);
  assert.match(all, /after = batch\.value\[batch\.value\.length - 1\]\.id/);
  assert.match(all, /batch\.value\.length === 0\) return \{ kind: "ok", value: \{ items, more: false \} \}/);
  assert.match(body(source("api.ts"), "memorySuggestions"), /loadAllProjectMemories\(project\.id\)[\s\S]*suggestionsFrom\(project, memories\.value\.items\)/);
  assert.match(source("screens/project/overview.tsx"), /loadAllProjectMemories\(data\.id\)\.then\(\(result\) => \{[^}]*memoryGroups\(result\.value\.items\)\.suggested\.length/);
  assert.match(source("screens/project/memory.tsx"), /loadAllProjectMemories\(data\.id\)/);
});

test("a Team panel row carries the engine's sentence for an agent the Delivery setting leaves without messages, and no other row does", () => {
  const note = "Can't receive messages: Native only needs tools on for Codex.";
  const panel = research.teamPanel({ agents: [teamAgent(2, "idle", { tools: false, message_note: note }), teamAgent(3, "idle")], tasks: [] }, [], (id) => id, "all", 0);
  assert.deepEqual(panel.agents.map((row) => [row.key, row.messageNote]), [["agent-2", note], ["agent-3", null]]);
});
