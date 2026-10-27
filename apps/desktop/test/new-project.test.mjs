import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

const wizard = await import("../src/screens/new-project/wizard.ts");

const available = { kind: "available" };
const noReader = { kind: "unavailable", reason: "CrazyAgent cannot read its replies yet" };
const harness = (id, name, status, tools = { kind: "switchable" }, kinds = ["subscription", "api_key"], team = available, aliases = []) => ({
  id, name, status, status_label: "", executable: null, headless: "", tools, model_selection: true, account_kinds: kinds, team, aliases,
});
const ready = { kind: "ready" };
const missing = { kind: "not_installed" };
const catalog = [
  harness("codex", "Codex", ready),
  harness("claude", "Claude Code", ready),
  harness("cursor", "Cursor CLI", { kind: "limited", reason: "no CrazyAgent launch adapter yet" }, { kind: "limited", reason: "tools can't be turned off" }, undefined, noReader),
  harness("gemini", "Gemini CLI", missing, undefined, undefined, noReader),
];
const engineTeam = (master, agents) => ({
  master, master_tools: true, sub_harnesses: agents ? [{ harness: master, agents, tools: true }] : [], min_agents: 1, max_agents: agents ? 1 + agents : wizard.TEAM_AGENTS_MAX,
});

test("a new draft starts on Basics with market, competitor and design research on", () => {
  const draft = wizard.emptyDraft();
  assert.deepEqual(draft.research, ["market_research", "competitor_research", "design_research"]);
  assert.equal(draft.pool, null);
  assert.equal(draft.poolChosen, false);
  assert.equal(draft.team, null);
  assert.deepEqual(wizard.STEPS.map((step) => step.label), ["Basics", "Documents", "Work", "Pool", "Team"]);
});

test("basics validation names each problem in plain words", () => {
  const draft = { ...wizard.emptyDraft(), name: "  " };
  assert.equal(wizard.basicsErrors(draft, []).name, "Enter a project name.");
  assert.equal(wizard.basicsErrors({ ...draft, name: "x".repeat(81) }, []).name, "Use at most 80 characters.");
  assert.equal(wizard.basicsErrors({ ...draft, name: "bad\u0007name" }, []).name, "Remove control characters from the name.");
  assert.equal(wizard.basicsErrors({ ...draft, name: " harbor notes " }, ["Harbor Notes"]).name, "You already have a project called Harbor Notes.");
  assert.equal(wizard.basicsErrors({ ...draft, name: "Harbor Notes", about: "a".repeat(4001) }, []).about, "Use at most 4000 characters.");
  assert.equal(wizard.basicsErrors({ ...draft, name: "Harbor Notes", platforms: ["other"], otherPlatform: "o".repeat(61) }, []).otherPlatform, "Use at most 60 characters.");
  assert.deepEqual(wizard.basicsErrors({ ...draft, name: "Harbor Notes", about: "Who it is for" }, ["Quayside"]), {});
  assert.equal(wizard.stepComplete("basics", { ...draft, name: "Harbor Notes" }, []), true);
  assert.equal(wizard.stepComplete("basics", draft, []), false);
});

test("tech stack entries are unique, bounded and custom entries are short", () => {
  let result = wizard.addStack([], "TypeScript");
  assert.deepEqual(result, { stack: ["TypeScript"] });
  result = wizard.addStack(result.stack, " typescript ");
  assert.deepEqual(result, { stack: ["TypeScript"] });
  assert.equal(wizard.addStack([], "x".repeat(41)).error, "Custom entries can be up to 40 characters.");
  assert.equal(wizard.addStack([], "   ").error, "Enter a name for the custom entry.");
  const full = Array.from({ length: 32 }, (_, i) => `t${i}`);
  assert.equal(wizard.addStack(full, "extra").error, "Pick at most 32 tech stack items.");
  assert.deepEqual(wizard.removeStack(["A", "B"], "A"), ["B"]);
});

test("documents are checked by type, name and count before the engine copies them", () => {
  const extensions = ["pdf", "md", "txt", "docx", "csv", "png", "jpg", "jpeg"];
  const added = wizard.addDocuments([], ["/Users/m/brief.PDF", "/Users/m/budget.xlsx", "relative/notes.md", "/Users/m/folder/"], extensions, 16);
  assert.deepEqual(added.documents, [{ path: "/Users/m/brief.PDF", name: "brief.PDF" }]);
  assert.deepEqual(added.errors, [
    "budget.xlsx: this file type isn't supported.",
    "notes.md can't be added. Choose it again with Choose files.",
    "Can't add a folder. Choose the files inside it.",
  ]);
  const again = wizard.addDocuments(added.documents, ["/Other/brief.PDF"], extensions, 16);
  assert.deepEqual(again.errors, ["brief.PDF is already attached."]);
  const many = Array.from({ length: 16 }, (_, i) => ({ path: `/d/${i}.md`, name: `${i}.md` }));
  assert.deepEqual(wizard.addDocuments(many, ["/d/x.md"], extensions, 16).errors, ["Attach at most 16 documents."]);
  assert.equal(wizard.formatBytes(52428800), "50 MB");
  assert.equal(wizard.formatBytes(1536), "1.5 KB");
});

test("team defaults keep the engine choice when its harness is ready", () => {
  const team = wizard.defaultTeam(engineTeam("claude", 3), catalog);
  assert.deepEqual(team, engineTeam("claude", 3));
  assert.equal(wizard.totalAgents(team), 4);
});

test("team defaults move to the first ready harness when the engine pick is not installed", () => {
  const team = wizard.defaultTeam(engineTeam("gemini", 2), catalog);
  assert.equal(team.master, "codex");
  assert.deepEqual(team.sub_harnesses, [{ harness: "codex", agents: 2, tools: true }]);
  assert.equal(team.max_agents, 3);
});

test("team defaults keep the engine plan while the harness catalog is not ready", () => {
  assert.deepEqual(wizard.defaultTeam(engineTeam("gemini", 2), null), engineTeam("gemini", 2));
});

test("team defaults with no research run any harness up to the team maximum and keep tools on", () => {
  const team = wizard.defaultTeam(engineTeam("codex", 0), catalog);
  assert.deepEqual(team.sub_harnesses, []);
  assert.equal(team.master_tools, true);
  assert.deepEqual([team.min_agents, team.max_agents], [1, 64]);
  assert.deepEqual(wizard.teamErrors(team, catalog), { rows: [] });
});

test("an empty agent list reads as any harness, and the total as up to the team maximum", () => {
  const any = engineTeam("codex", 0);
  assert.equal(wizard.ANY_HARNESS_NOTE, "Any harness, no limit. Add a harness to limit which ones run and how many.");
  assert.equal(wizard.anyHarnessTotal(any), "Any harness, up to 64 agents");
  assert.equal(wizard.agentTotalText(any), "This team has up to 64 agents.");
  assert.equal(wizard.agentTotalText({ ...any, max_agents: 5 }), "This team has up to 5 agents.");
  const chosen = wizard.addHarnessRow(any, catalog);
  assert.deepEqual(chosen.sub_harnesses, [{ harness: "codex", agents: 1, tools: true }]);
  assert.equal(wizard.agentTotalText(chosen), "This team has 2 agents.");
  assert.equal(wizard.agentTotalText({ ...engineTeam("codex", 0), sub_harnesses: [], max_agents: 1 }), "This team has up to 1 agent.");
});

test("a harness whose tools can't be turned off keeps tools on", () => {
  const team = wizard.setTools({ ...engineTeam("codex", 1), sub_harnesses: [{ harness: "cursor", agents: 1, tools: true }] }, 0, false, catalog);
  assert.equal(team.sub_harnesses[0].tools, true);
  const codex = wizard.setTools(engineTeam("codex", 1), 0, false, catalog);
  assert.equal(codex.sub_harnesses[0].tools, false);
  const master = wizard.setTools(engineTeam("codex", 1), "master", false, catalog);
  assert.equal(master.master_tools, false);
  assert.deepEqual(wizard.teamNotes({ ...engineTeam("codex", 1), sub_harnesses: [{ harness: "cursor", agents: 1, tools: true }] }, catalog), [
    "Cursor CLI: tools can't be turned off.",
    "Cursor CLI: no CrazyAgent launch adapter yet.",
  ]);
});

test("switching to a harness whose tools can't be turned off and back restores the owner's choice", () => {
  const off = wizard.setTools(wizard.setTools(engineTeam("codex", 1), "master", false, catalog), 0, false, catalog);
  assert.deepEqual([off.master_tools, off.sub_harnesses[0].tools], [false, false]);
  const forced = wizard.setRowHarness(wizard.setMaster(off, "cursor", catalog), 0, "cursor", catalog);
  assert.deepEqual([forced.master_tools, forced.sub_harnesses[0].tools], [true, true]);
  const back = wizard.setRowHarness(wizard.setMaster(forced, "claude", catalog), 0, "claude", catalog);
  assert.deepEqual([back.master, back.master_tools, back.sub_harnesses[0].harness, back.sub_harnesses[0].tools], ["claude", false, "claude", false]);
  const on = wizard.setRowHarness(wizard.setMaster(wizard.setMaster(engineTeam("codex", 1), "cursor", catalog), "codex", catalog), 0, "claude", catalog);
  assert.deepEqual([on.master_tools, on.sub_harnesses[0].tools], [true, true]);
  const request = wizard.buildRequest({ ...wizard.emptyDraft(), name: "Harbor", team: forced }).setup.team;
  assert.deepEqual(request, {
    master: "cursor", master_tools: true, sub_harnesses: [{ harness: "cursor", agents: 1, tools: true }], min_agents: 1, max_agents: 2,
  });
  assert.deepEqual(wizard.wireTeam(back), {
    master: "claude", master_tools: false, sub_harnesses: [{ harness: "claude", agents: 1, tools: false }], min_agents: 1, max_agents: 2,
  });
});

test("harness choices list every harness and say why one can't be picked", () => {
  assert.deepEqual(wizard.harnessChoices(catalog).map((choice) => [choice.id, choice.group, choice.disabled]), [
    ["codex", "Ready", false],
    ["claude", "Ready", false],
    ["cursor", "Limited control", true],
    ["gemini", "Not installed", true],
  ]);
  assert.equal(wizard.harnessChoices(catalog)[2].label, "Cursor CLI - Can't run team agents");
  assert.equal(wizard.harnessChoices(catalog)[3].label, "Gemini CLI - Not installed");
});

test("team validation mirrors the engine agent range", () => {
  const ok = engineTeam("codex", 3);
  assert.deepEqual(wizard.teamErrors(ok, catalog), { rows: [undefined] });
  assert.equal(wizard.teamErrors({ ...ok, min_agents: 0 }, catalog).range, "At most must be at least 1 and not less than At least.");
  assert.equal(wizard.teamErrors({ ...ok, min_agents: 5, max_agents: 4 }, catalog).range, "At most must be at least 1 and not less than At least.");
  assert.equal(wizard.teamErrors({ ...ok, max_agents: 3 }, catalog).range, "This team has 4 agents: the master and 3 more. Set At most to 4 or more.");
  assert.equal(wizard.teamErrors({ ...ok, min_agents: 5, max_agents: 6 }, catalog).range, "This team has 4 agents. Set At least to 4 or less.");
  assert.equal(wizard.teamErrors({ ...ok, max_agents: 65 }, catalog).range, "At most can be up to 64.");
  assert.equal(wizard.teamErrors({ ...ok, master: "gemini" }, catalog).master, "Gemini CLI isn't installed on this Mac. Pick another harness.");
  assert.equal(wizard.teamErrors({ ...ok, master: "cursor" }, catalog).master, "Cursor CLI can't run team agents yet. Pick another harness.");
  assert.deepEqual(wizard.teamErrors({ ...ok, sub_harnesses: [{ harness: "cursor", agents: 1, tools: true }] }, catalog).rows, ["Cursor CLI can't run team agents yet. Pick another harness."]);
  assert.equal(wizard.teamErrors({ ...ok, master: "" }, catalog).master, "Pick a harness for the master.");
  assert.deepEqual(wizard.teamErrors({ ...ok, sub_harnesses: [{ harness: "codex", agents: 16, tools: true }], max_agents: 17 }, catalog).rows, ["Each harness runs 1 to 15 agents."]);
  assert.deepEqual(wizard.teamErrors({ ...ok, sub_harnesses: [{ harness: "codex", agents: 1.5, tools: true }] }, catalog).rows, ["Each harness runs 1 to 15 agents."]);
  const nine = Array.from({ length: 9 }, () => ({ harness: "codex", agents: 1, tools: true }));
  assert.equal(wizard.teamErrors({ ...ok, sub_harnesses: nine, max_agents: 10 }, catalog).range, "Use at most 8 agent harnesses.");
  assert.equal(wizard.teamValid(ok, catalog), true);
  assert.equal(wizard.teamValid({ ...ok, max_agents: 2 }, catalog), false);
});

test("adding and removing agent rows keeps the agent range valid", () => {
  let team = engineTeam("codex", 1);
  team = wizard.addHarnessRow(team, catalog);
  assert.deepEqual(team.sub_harnesses.map((slot) => slot.harness), ["codex", "claude"]);
  assert.equal(team.max_agents, 3);
  team = wizard.setAgents(team, 1, 4);
  assert.equal(team.sub_harnesses[1].agents, 4);
  assert.equal(team.max_agents, 6);
  team = wizard.removeHarnessRow(team, 0);
  assert.deepEqual(team.sub_harnesses, [{ harness: "claude", agents: 4, tools: true }]);
  assert.equal(wizard.teamValid(team, catalog), true);
  assert.equal(wizard.setAgents(team, 0, 99).sub_harnesses[0].agents, 15);
  assert.equal(wizard.setAgents(team, 0, 0).sub_harnesses[0].agents, 1);
});

test("the request carries every answer and no repositories", () => {
  const draft = {
    ...wizard.emptyDraft(), name: " Harbor Notes ", about: "Notes for marine engineers.", stack: ["Rust", "React"],
    platforms: ["mobile_app", "other"], otherPlatform: "Smart watch", documents: [{ path: "/d/brief.pdf", name: "brief.pdf" }],
    research: ["market_research"], pool: "main", poolChosen: true, team: engineTeam("codex", 1),
  };
  assert.deepEqual(wizard.buildRequest(draft), {
    spec: { name: "Harbor Notes", repositories: [] },
    setup: {
      about: "Notes for marine engineers.\n\nOther platform: Smart watch", tech_stack: ["Rust", "React"],
      platforms: ["mobile_app", "other"], research: ["market_research"], pool: "main", team: engineTeam("codex", 1), documents: ["/d/brief.pdf"],
    },
  });
  assert.equal(wizard.buildRequest({ ...draft, platforms: ["website"], otherPlatform: "ignored" }).setup.about, "Notes for marine engineers.");
});

test("the summary says how many research agents start and where", () => {
  const draft = { ...wizard.emptyDraft(), name: "Harbor Notes" };
  assert.equal(wizard.summary(draft, "Main pool"), "Creating Harbor Notes starts 3 research agents on Main pool.");
  assert.equal(wizard.summary({ ...draft, research: ["market_research"] }, "Main pool"), "Creating Harbor Notes starts 1 research agent on Main pool.");
  assert.equal(wizard.summary(draft, null), "Creating Harbor Notes saves it. Research starts when you choose a pool.");
  assert.equal(wizard.summary({ ...draft, research: [] }, "Main pool"), "Creating Harbor Notes saves it. No research is picked.");
});

test("pools are summarised by provider and the biggest pool is preselected", () => {
  const member = (id, harness, auth, url) => ({ id, provider: "p", harness, protocol: "x", account_ref: id, endpoint: url ? { kind: "http", url } : { kind: "native" }, model: { id: "m", author: "a" }, auth });
  const managed = { method: "native_managed", profile_ref: "r" };
  const members = [member("a", "codex", managed), member("b", "codex", managed), member("c", "claude", managed),
    member("d", "codex", { method: "api_key", credential_ref: "k" }, "https://api.openai.com/v1"), member("e", "codex", { method: "none" }, "http://127.0.0.1:11434/v1")];
  assert.equal(wizard.poolSummary(members), "2 ChatGPT, 1 Claude, 1 API key, 1 local model");
  assert.equal(wizard.poolSummary([]), "No accounts");
  const pools = [{ pool: { id: "small", members: ["a"] }, members: [] }, { pool: { id: "big", members: ["a", "b"] }, members: [] }];
  assert.equal(wizard.preselectPool(pools), "big");
  assert.equal(wizard.preselectPool([]), null);
  assert.equal(wizard.poolFits(catalog[0], members), true);
  assert.equal(wizard.poolFits(harness("opencode", "OpenCode", ready, undefined, ["api_key"]), [member("a", "codex", managed)]), false);
});

test("Claude subscriptions fit Claude Code under the catalog id and aliases the engine serves", () => {
  const claudeCode = harness("claude-code", "Claude Code", ready, undefined, ["subscription", "api_key", "local"], available, ["claude"]);
  const managed = { method: "native_managed", profile_ref: "profile-claude" };
  const account = (id, harnessName) => ({ id, provider: "anthropic", harness: harnessName, protocol: "native", account_ref: id, endpoint: { kind: "native" }, model: { id: "claude", author: "anthropic" }, auth: managed });
  const claudes = ["c1", "c2", "c3", "c4"].map((id) => account(id, "claude"));
  assert.equal(wizard.poolFits(claudeCode, claudes), true);
  assert.equal(wizard.poolFits(claudeCode, [account("c5", "claude-code")]), true);
  assert.equal(wizard.poolFits(claudeCode, [account("x1", "codex")]), false);
  assert.equal(wizard.poolFits(harness("codex", "Codex", ready), claudes), false);
});

test("a pool shows its name from the engine and falls back to its id", () => {
  const pool = (extra) => ({ id: "pool-0a1b2c", members: [], projects: [], roles: [], fallback_allowed: false, ...extra });
  assert.equal(wizard.poolName(pool({ name: "Main pool" })), "Main pool");
  assert.equal(wizard.poolName(pool({ name: "  Main pool  " })), "Main pool");
  assert.equal(wizard.poolName(pool({ name: "   " })), "pool-0a1b2c");
  assert.equal(wizard.poolName(pool({})), "pool-0a1b2c");
});

test("ready accounts are counted and the most ready pool is preselected", () => {
  const member = (id, status) => ({ id, kind: "chatgpt", provider: "openai", status, status_label: "" });
  const summary = (id, statuses) => ({ id, name: id, summary: "", members: statuses.map((status, index) => member(`${id}-${index}`, status)), project_count: 0 });
  assert.equal(wizard.readyCount(summary("p", ["signed_in", "not_signed_in", "set_up"])), 2);
  const pools = [{ pool: { id: "big", members: ["a", "b", "c"] }, members: [] }, { pool: { id: "ready", members: ["d", "e"] }, members: [] }];
  const summaries = [summary("big", ["not_signed_in", "not_signed_in", "signed_in"]), summary("ready", ["signed_in", "set_up"])];
  assert.equal(wizard.preselectPool(pools, summaries), "ready");
  assert.equal(wizard.preselectPool(pools), "big");
  assert.equal(wizard.preselectPool(pools, []), "big");
});

test("an attached document shows its type from the extension", () => {
  assert.equal(wizard.documentType("notes.md"), "Markdown");
  assert.equal(wizard.documentType("Pitch.PDF"), "PDF");
  assert.equal(wizard.documentType("brief.docx"), "Word");
  assert.equal(wizard.documentType("rows.csv"), "CSV");
  assert.equal(wizard.documentType("shot.jpeg"), "JPG");
  assert.equal(wizard.documentType("shot.jpg"), "JPG");
  assert.equal(wizard.documentType("logo.png"), "PNG");
  assert.equal(wizard.documentType("read.txt"), "Text");
  assert.equal(wizard.documentType("archive.tar.gz"), "GZ");
  assert.equal(wizard.documentType("Makefile"), "File");
});

const stored = {
  project_id: "p-1", name: "Harbor Notes", about: "Notes for marine engineers.", tech_stack: ["Rust", "SQLite"], platforms: ["desktop_app"],
  research: ["market_research"], pool: "main-pool", team: { master: "codex", master_tools: true, sub_harnesses: [{ harness: "codex", agents: 2, tools: true }], min_agents: 1, max_agents: 3 },
  documents: [{ name: "spec.pdf", bytes: 10 }], folder: "/Users/me/CrazyAgent/harbor-notes", created_ms: 0, revision: 4,
};

test("project Settings starts from the saved project and sends nothing until something changes", () => {
  const draft = wizard.draftOf(stored);
  assert.deepEqual(draft, {
    name: "Harbor Notes", about: "Notes for marine engineers.", stack: ["Rust", "SQLite"], platforms: ["desktop_app"], otherPlatform: "", documents: [],
    research: ["market_research"], pool: "main-pool", poolChosen: true, team: stored.team,
  });
  assert.notEqual(draft.stack, stored.tech_stack);
  assert.equal(wizard.editRequest(stored, draft).changed, false);
  assert.equal(wizard.editRequest(stored, { ...draft, name: "  Harbor Notes ", about: "Notes for marine engineers.  " }).changed, false);
});

test("an edited project keeps its pool and team, adds no documents and renames only when the name changed", () => {
  const draft = wizard.draftOf(stored);
  const edited = wizard.editRequest(stored, { ...draft, about: "For harbor crews.", stack: ["Rust"], research: ["market_research", "competitor_research"] });
  assert.deepEqual(edited, {
    name: null, changed: true,
    setup: { about: "For harbor crews.", tech_stack: ["Rust"], platforms: ["desktop_app"], research: ["market_research", "competitor_research"], pool: "main-pool", team: stored.team, documents: [] },
  });
  assert.equal(wizard.editRequest(stored, { ...draft, name: " Harbor Log " }).name, "Harbor Log");
  assert.equal(wizard.editRequest(stored, { ...draft, research: [] }).changed, true);
  assert.equal(wizard.editRequest(stored, { ...draft, platforms: ["desktop_app", "website"] }).changed, true);
  const other = wizard.editRequest(stored, { ...draft, platforms: ["desktop_app", "other"], otherPlatform: "Kiosk" });
  assert.equal(other.setup.about, "Notes for marine engineers.\n\nOther platform: Kiosk");
  assert.deepEqual(wizard.basicsErrors({ ...draft, name: "Harbor Notes" }, ["Docs Site"]), {});
  assert.equal(wizard.basicsErrors({ ...draft, name: "docs site" }, ["Docs Site"]).name, "You already have a project called Docs Site.");
});


test("a pool with one account runs every agent of the team and never tells them to wait for it", async () => {
  const { createElement } = await import("react");
  const { renderToStaticMarkup } = await import("react-dom/server");
  const { TeamEditor } = await import("../src/screens/new-project/team-editor.tsx");
  const managed = { method: "native_managed", profile_ref: "r" };
  const member = { id: "solo", provider: "p", harness: "codex", protocol: "x", account_ref: "solo", endpoint: { kind: "native" }, model: { id: "m", author: "a" }, auth: managed };
  const pool = { pool: { id: "solo-pool", name: "Solo", members: ["solo"], projects: [], roles: [], fallback_allowed: false }, members: [member] };
  const team = engineTeam("codex", 2);
  assert.equal(team.max_agents, 3);
  const html = renderToStaticMarkup(createElement(TeamEditor, { team, harnesses: catalog, pool, research: [], onChange: () => {}, prefix: "np" }));
  assert.match(html, /Uses an account from Solo\./);
  assert.match(html, /This team has 3 agents\./);
  assert.doesNotMatch(html, /wait for a free account/i);
  assert.doesNotMatch(html, /Agents beyond/);
});
