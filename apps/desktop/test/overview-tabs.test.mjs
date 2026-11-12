import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const view = await import("../src/screens/project/overview-tabs.tsx");
const screen = await import("../src/screens/project/overview.tsx");
const api = await import("../src/api-overview.ts");

const TAB_IDS = ["quick_info", "features", "competitors", "install_and_use"];
const NOW = 10 * 60_000;

const base = () => ({
  readme: null, tabs: TAB_IDS.map((tab) => ({ tab, text: null, version: "", edited: false, proposal: null, proposal_version: "", blocked: null })),
  competitors: { kind: "missing" }, running: false, stale: false, checked_ms: null, failure: null, refresh: "after_landing", apply: "always_ask", needs_home: false,
});
const blockedTab = (overview, id, blocked) => ({ ...overview, tabs: overview.tabs.map((tab) => tab.tab === id ? { ...tab, blocked } : tab) });
const withTab = (overview, id, text) => ({ ...overview, tabs: overview.tabs.map((tab) => tab.tab === id ? { ...tab, text, version: `v-${id}` } : tab) });
const job = (kind, status) => ({
  id: 1, project_id: "alpha", kind, kind_label: "Competitor research", status, status_label: "", report_path: `context/research/${kind}.md`,
  harness: "codex", tools: true, attempts: 0, claimer: null, error: null, cancel_requested: false, output_bytes: 0,
  created_ms: 1_000, started_ms: null, heartbeat_ms: null, finished_ms: null,
});
const stored = (research) => ({
  project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research, pool: null,
  team: { master: "codex", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 }, documents: [], folder: "/tmp/alpha", created_ms: 0, revision: 4,
});
const projectData = (jobs = [], research = ["market_research"]) => ({
  id: "alpha", name: "Alpha", setup: { kind: "ok", value: stored(research) }, research: { kind: "ok", value: { project_id: "alpha", jobs } }, context: undefined,
  pools: undefined, harnesses: undefined, assigned: null, now: NOW, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
  team: { result: undefined, at: 0 }, reloadTeam: () => {},
});
const idle = () => Promise.resolve({ kind: "ok", value: undefined });
const actions = { refresh: idle, save: idle, accept: idle, reject: idle, reload: () => {} };

function page({ selected, overview, about = "", mappingOverview = null, researchRunning = false, jobs, research, runMode = "off", problem, store }) {
  const data = projectData(jobs ?? (researchRunning ? [job("competitor_research", "running")] : []), research ?? (researchRunning || jobs ? ["market_research", "competitor_research"] : ["market_research"]));
  return renderToStaticMarkup(createElement(view.OverviewTabs, {
    data, overview, problem, now: NOW, selected, onSelect: () => {}, actions, runMode,
    quickInfo: (text) => createElement(screen.OverviewFacts, { about, overview: text ?? mappingOverview }),
    activity: createElement("p", null, "Today's activity"), store,
  }));
}

test("the strip lists Quick info first, then Features, Competitors, Install & use, README and Activity", () => {
  assert.deepEqual(view.STRIP.map((item) => item.label), ["Quick info", "Features", "Competitors", "Install & use", "README", "Activity"]);
  assert.deepEqual(view.STRIP.map((item) => item.id), ["quick_info", "features", "competitors", "install_and_use", "readme", "activity"]);
  const html = page({ selected: "features", overview: base() });
  assert.match(html, /role="tablist"/);
  assert.equal((html.match(/role="tab"/g) ?? []).length, 6);
  assert.match(html, /<button[^>]*role="tab"[^>]*aria-selected="true"[^>]*>Features<\/button>/);
  assert.equal((html.match(/aria-selected="true"/g) ?? []).length, 1);
});

test("Quick info shows the saved About and the generated quick info through OverviewFacts", () => {
  const html = page({ selected: "quick_info", about: "A research reader.", overview: withTab(base(), "quick_info", "Alpha reads papers.\n") });
  assert.match(html, /A research reader\./);
  assert.match(html, /Alpha reads papers\./);
});

test("Quick info falls back to the mapping's overview until the first run wrote one", () => {
  const html = page({ selected: "quick_info", about: "A research reader.", mappingOverview: "# ScholarScope\n\nReads papers.\n", overview: base() });
  assert.match(html, /Reads papers\./);
});

test("the README tab renders the repo file, names it and says it is read only", () => {
  const html = page({ selected: "readme", overview: { ...base(), readme: { path: "README.md", text: "# Alpha\n\nReads papers.\n", cut: false } } });
  assert.match(html, /Reads papers\./);
  assert.match(html, /README\.md/);
  assert.match(html, /This is the project&#x27;s own file\. Edit it in the project\./);
  assert.doesNotMatch(html, />Edit</);
});

test("a cut README says only the first part is shown", () => {
  const html = page({ selected: "readme", overview: { ...base(), readme: { path: "README.md", text: "# Alpha\n", cut: true } } });
  assert.match(html, /Showing the first part of a long file\./);
});

test("a project without a README says so", () => {
  assert.match(page({ selected: "readme", overview: base() }), /This project has no README file\./);
});

test("a tab not written yet says what will write it, by the refresh setting", () => {
  assert.match(page({ selected: "features", overview: { ...base(), running: true } }), /Writing now/);
  assert.match(page({ selected: "features", overview: { ...base(), refresh: "refresh_only" } }), /Not written yet\. Choose Refresh to write it\./);
  assert.match(page({ selected: "features", overview: { ...base(), refresh: "off" } }), /Overview tabs are off in Project settings\./);
  assert.match(page({ selected: "features", overview: base() }), /Not written yet\. It is written after the next landed change set, or when you choose Refresh\./);
});

test("a refresh the engine queued says it is waiting to run, read from the engine, so it shows again when the view opens again", () => {
  const queued = { ...base(), stale: true, checked_ms: NOW - 60 * 60_000 };
  const html = page({ selected: "features", overview: queued });
  assert.match(html, />Refresh waiting to run</);
  assert.match(html, /A refresh is waiting to run\. This tab fills in when it ends\./);
  assert.doesNotMatch(html, /after the next landed change set/);
  assert.doesNotMatch(html, /Checked/);
  for (const quiet of [{ ...queued, refresh: "off" }, { ...queued, running: true }, { ...queued, stale: false }]) {
    assert.doesNotMatch(page({ selected: "features", overview: quiet }), /waiting to run/);
  }
});

test("Refresh is disabled while a run is going and when the tabs are off", () => {
  assert.match(page({ selected: "features", overview: { ...base(), running: true } }), /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.match(page({ selected: "features", overview: { ...base(), refresh: "off" } }), /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.doesNotMatch(page({ selected: "features", overview: base() }), /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.match(page({ selected: "features", overview: base() }), />Refresh<\/button>/);
});

test("a failed run shows its plain reason and keeps the tab text", () => {
  const html = page({ selected: "features", overview: { ...withTab(base(), "features", "- Search\n"), failure: "The account is out of usage" } });
  assert.match(html, /Couldn&#x27;t refresh the tabs\. The account is out of usage/);
  assert.match(html, /Search/);
});

test("the strip says when the tabs were last checked and that a run is writing", () => {
  assert.match(page({ selected: "features", overview: { ...base(), checked_ms: NOW - 3 * 60_000 } }), /Checked 3 min ago/);
  assert.match(page({ selected: "readme", overview: { ...base(), running: true } }), /Writing now/);
  assert.doesNotMatch(page({ selected: "features", overview: base() }), /Checked/);
});

test("the Activity tab holds the activity panel, and README and Activity stay when the tabs could not be read", () => {
  assert.match(page({ selected: "activity", overview: base() }), /Today&#x27;s activity/);
  const lost = page({ selected: "activity", overview: null, problem: "Couldn't load the Overview tabs. The engine is busy." });
  assert.match(lost, /Today&#x27;s activity/);
  assert.equal((lost.match(/role="tab"/g) ?? []).length, 6);
  assert.match(page({ selected: "features", overview: null, problem: "Couldn't load the Overview tabs. The engine is busy." }), /Couldn&#x27;t load the Overview tabs\. The engine is busy\./);
  assert.match(page({ selected: "features", overview: null }), /Loading the Overview tabs/);
});

test("an arrow key moves to the next tab and wraps, Home and End jump", () => {
  assert.equal(view.stripMove("quick_info", "ArrowRight"), "features");
  assert.equal(view.stripMove("quick_info", "ArrowLeft"), "activity");
  assert.equal(view.stripMove("activity", "ArrowRight"), "quick_info");
  assert.equal(view.stripMove("features", "Home"), "quick_info");
  assert.equal(view.stripMove("features", "End"), "activity");
  assert.equal(view.stripMove("features", "a"), null);
});

test("the picked tab is kept per project and a missing or unknown stored value reads as Quick info", () => {
  const stored = new Map();
  const store = { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  assert.equal(view.storedTab("alpha", store), "quick_info");
  view.storeTab("alpha", "readme", store);
  assert.equal(view.storedTab("alpha", store), "readme");
  assert.equal(view.storedTab("beta", store), "quick_info");
  stored.set([...stored.keys()][0], "nonsense");
  assert.equal(view.storedTab("alpha", store), "quick_info");
  const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
  assert.equal(view.storedTab("alpha", broken), "quick_info");
  assert.doesNotThrow(() => view.storeTab("alpha", "readme", broken));
});

test("the tabs are read again only while a run is going or a change set landed", () => {
  assert.equal(view.pollEvery({ ...base(), running: true }), api.OVERVIEW_POLL_MS);
  assert.equal(view.pollEvery({ ...base(), stale: true }), api.OVERVIEW_POLL_MS);
  assert.equal(view.pollEvery({ ...base(), stale: true, refresh: "refresh_only" }), api.OVERVIEW_POLL_MS);
  assert.equal(view.pollEvery({ ...base(), stale: true, refresh: "off" }), null, "no run comes while the tabs are off, so nothing is waited for");
  assert.equal(view.pollEvery(base()), null);
  assert.equal(view.pollEvery(null), null);
  assert.equal(api.OVERVIEW_POLL_MS, 5_000);
});

test("every Overview call names its bridge command, and each command is in the handler list", () => {
  const source = readFileSync(new URL("../src/api-overview.ts", import.meta.url), "utf8");
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const command of ["desktop_overview_read", "desktop_overview_refresh", "desktop_overview_save", "desktop_overview_accept", "desktop_overview_reject", "desktop_overview_set_refresh", "desktop_overview_set_apply"]) {
    assert.match(source, new RegExp(`"${command}"`), command);
    assert.match(lib, new RegExp(`async fn ${command}\\(`), command);
    assert.match(lib, new RegExp(`\\n    ${command},\\n`), `${command} is in the handler list`);
  }
});

test("every Overview refusal has its own plain sentence", () => {
  const codes = ["overview_tab_changed", "overview_tab_blocked", "overview_tab_too_large", "overview_tab_empty", "overview_no_proposal", "overview_proposal_changed", "overview_off", "overview_project_unknown", "overview_unavailable", "overview_needs_project_home"];
  for (const code of codes) {
    assert.notEqual(api.overviewCopy(code), api.overviewCopy("something_else"), code);
    assert.doesNotMatch(api.overviewCopy(code), /overview_|\u{2014}/u, code);
  }
  assert.equal(new Set(codes.map((code) => api.overviewCopy(code))).size, codes.length, "no two refusals share a sentence");
  assert.equal(api.overviewCopy("overview_tab_changed"), "Someone changed this tab since you opened it. Your text is kept below; compare and save again.");
});

test("Competitors without research explains and offers Run research, and nothing starts on its own", () => {
  const html = page({ selected: "competitors", overview: base() });
  assert.match(html, /There is no competitor research for this project yet\. This tab is written from it: who the competitors are, what differs, where this project is better and where it is not\./);
  assert.match(html, /<button[^>]*>Run research<\/button>/);
});

test("Run research turns competitor research on through the project setup save, then reads the project again", async () => {
  const saved = [];
  const told = [];
  let reloads = 0;
  const data = { ...projectData(), notify: (text) => told.push(text), reload: () => { reloads += 1; } };
  const applied = { kind: "ok", value: { outcome: "applied", pending: false } };
  const note = await view.runResearch(data, (id, research) => { saved.push([id, research]); return Promise.resolve(applied); });
  assert.deepEqual(saved, [["alpha", ["market_research", "competitor_research"]]]);
  assert.equal(note, "");
  assert.equal(reloads, 1);
  assert.equal(told.length, 1);
  assert.match(told[0], /Competitor research is on\./);
});

test("a refused Run research says why and does not read the project again", async () => {
  let reloads = 0;
  const data = { ...projectData(), reload: () => { reloads += 1; } };
  const note = await view.runResearch(data, () => Promise.resolve({ kind: "error", code: "setup_revision_changed" }));
  assert.notEqual(note, "");
  assert.equal(reloads, 0);
  assert.notEqual(await view.runResearch(data, () => Promise.resolve({ kind: "not_ready" })), "");
});

test("Competitors with linked research names the files, and with no tab yet says it is written on the next refresh", () => {
  const html = page({ selected: "competitors", overview: { ...base(), competitors: { kind: "linked", paths: ["docs/research-competitors.md"] } } });
  assert.match(html, /docs\/research-competitors\.md/);
  assert.match(html, /Choose Refresh to write this tab from it\./);
  assert.doesNotMatch(html, />Run research</);
});

test("Competitors with a research report offers the report and shows the tab", () => {
  const overview = { ...withTab(base(), "competitors", "- Beta: cheaper, no offline mode\n"), competitors: { kind: "report", path: "context/research/competitor_research.md" } };
  const html = page({ selected: "competitors", overview });
  assert.match(html, /Written from the competitor research report\./);
  assert.match(html, /<button[^>]*>Open report<\/button>/);
  assert.match(html, /Beta: cheaper, no offline mode/);
  assert.doesNotMatch(html, />Run research</);
});

test("Competitors while research runs, waits for a pool or did not finish says so and hides Run research", () => {
  const running = page({ selected: "competitors", overview: base(), researchRunning: true });
  assert.match(running, /Competitor research is running\./);
  assert.doesNotMatch(running, />Run research</);
  const queued = page({ selected: "competitors", overview: base(), jobs: [job("competitor_research", "queued")] });
  assert.match(queued, /Competitor research is running\./);
  const waiting = page({ selected: "competitors", overview: base(), jobs: [job("competitor_research", "needs_pool")] });
  assert.match(waiting, /Competitor research is waiting for a pool\./);
  assert.doesNotMatch(waiting, />Run research</);
  const failed = page({ selected: "competitors", overview: base(), jobs: [job("market_research", "done"), job("competitor_research", "failed")] });
  assert.match(failed, /Competitor research didn&#x27;t finish\. Try it again on the Activity tab\./);
  assert.match(failed, /<button[^>]*>Open Activity<\/button>/);
  assert.doesNotMatch(failed, />Run research</);
});

test("each command line on Install & use gets Copy and Run in project terminal", () => {
  const overview = withTab(base(), "install_and_use", "## Install\n\n```sh\ncargo install alpha\nalpha init\n```\n");
  const html = page({ selected: "install_and_use", overview, runMode: "type_and_run" });
  assert.equal((html.match(/>Copy</g) ?? []).length, 2);
  assert.equal((html.match(/>Run in project terminal</g) ?? []).length, 2);
  assert.match(html, /<code>cargo install alpha<\/code>/);
  assert.match(html, /<code>alpha init<\/code>/);
});

test("a line with shell syntax gets Copy only and says why", () => {
  const overview = withTab(base(), "install_and_use", "```sh\ncurl -fsSL https://example.com/install.sh | sh\n```\n");
  const html = page({ selected: "install_and_use", overview, runMode: "type_and_run" });
  assert.match(html, />Copy</);
  assert.doesNotMatch(html, />Run in project terminal</);
  assert.match(html, /This line has shell syntax CrazyAgent won&#x27;t type for you\. Copy it and run it yourself\./);
});

test("with Run in project terminal off, commands get Copy only", () => {
  const overview = withTab(base(), "install_and_use", "```sh\ncargo install alpha\n```\n");
  const html = page({ selected: "install_and_use", overview, runMode: "off" });
  assert.match(html, />Copy</);
  assert.doesNotMatch(html, />Run in project terminal</);
});

test("command lines skip blank lines, comments and a leading prompt sign, and stop at the bound", () => {
  assert.deepEqual(view.commandLines("$ cargo build\n\n  alpha init  \n"), ["cargo build", "alpha init"]);
  assert.deepEqual(view.commandLines("# build it\ncargo build\n   # then\n"), ["cargo build"]);
  assert.deepEqual(view.commandLines("$ $ odd\n"), ["$ odd"], "only one prompt sign is dropped");
  assert.deepEqual(view.commandLines(""), []);
  assert.equal(view.COMMAND_LINES_MAX, 200);
  assert.equal(view.commandLines("ls\n".repeat(view.COMMAND_LINES_MAX + 5)).length, view.COMMAND_LINES_MAX);
});

test("a command line with a hidden character is the shown text, so Copy takes what the page shows and Run is not offered", () => {
  const hidden = `echo a${String.fromCodePoint(0x200b)}b`;
  const shown = `echo a${String.fromCharCode(92)}u200bb`;
  assert.deepEqual(view.commandLines(hidden), [shown]);
  const html = page({ selected: "install_and_use", overview: withTab(base(), "install_and_use", `~~~sh\n${hidden}\n~~~\n`), runMode: "type_and_run" });
  assert.match(html, />Copy</);
  assert.doesNotMatch(html, />Run in project terminal</);
  assert.ok(!html.includes(String.fromCodePoint(0x200b)), "the hidden character is not in the page");
});

test("a code block on the README tab gets Copy only", () => {
  const html = page({ selected: "readme", overview: { ...base(), readme: { path: "README.md", text: "```sh\ncargo install alpha\n```\n", cut: false } }, runMode: "type_and_run" });
  assert.match(html, />Copy</);
  assert.doesNotMatch(html, />Run in project terminal</);
});

test("only shell blocks get command actions, and a console block only on its prompt lines", () => {
  const data = withTab(base(), "install_and_use", "```json\n{ \"name\": \"alpha\" }\n```\n\n```\nplain text\n```\n");
  assert.doesNotMatch(page({ selected: "install_and_use", overview: data, runMode: "type_and_run" }), />Copy</);
  const session = withTab(base(), "install_and_use", "```console\n$ alpha --version\nalpha 1.2.0\n```\n");
  const html = page({ selected: "install_and_use", overview: session, runMode: "type_and_run" });
  assert.equal((html.match(/>Copy</g) ?? []).length, 1);
  assert.equal((html.match(/>Run in project terminal</g) ?? []).length, 1);
  assert.match(html, /alpha 1\.2\.0/);
  for (const language of ["sh", "bash", "zsh", "shell", "SH"]) {
    const block = withTab(base(), "install_and_use", `\`\`\`${language}\nalpha init\n\`\`\`\n`);
    assert.equal((page({ selected: "install_and_use", overview: block, runMode: "type_and_run" }).match(/>Copy</g) ?? []).length, 1, language);
  }
});

test("a blocked tab says why in plain words, names its file and what to do", () => {
  const blank = page({ selected: "features", overview: blockedTab(base(), "features", "blank") });
  assert.match(blank, /CrazyAgent can&#x27;t show this tab: its file is empty\./);
  assert.match(blank, /\.crazyagent\/overview\/features\.md/);
  assert.match(blank, /Fix or delete the file, then choose Refresh\./);
  assert.match(page({ selected: "features", overview: blockedTab(base(), "features", "too_large") }), new RegExp(`CrazyAgent can&#x27;t show this tab: its file is larger than ${view.TAB_BYTES_MAX / 1024} KB\\.`));
  assert.match(page({ selected: "install_and_use", overview: blockedTab(base(), "install_and_use", "not_utf8") }), /CrazyAgent can&#x27;t show this tab: its file is not plain text\./);
  assert.match(page({ selected: "install_and_use", overview: blockedTab(base(), "install_and_use", "not_utf8") }), /\.crazyagent\/overview\/install-and-use\.md/);
  const unknown = page({ selected: "features", overview: blockedTab(base(), "features", "something_new") });
  assert.match(unknown, /CrazyAgent can&#x27;t show this tab: its file can&#x27;t be read\./);
  assert.doesNotMatch(unknown, /something_new/);
  assert.doesNotMatch(blank, /Not written yet/);
});

test("a tab whose place holds a link or folder says so, names the path and asks to remove it", () => {
  const html = page({ selected: "features", overview: blockedTab(base(), "features", "not_a_file") });
  assert.match(html, /CrazyAgent can&#x27;t show this tab: a link or folder is where its file goes\./);
  assert.match(html, /\.crazyagent\/overview\/features\.md/);
  assert.match(html, /Remove it, then choose Refresh\./);
  assert.doesNotMatch(html, /Fix or delete the file/);
  assert.doesNotMatch(html, />Edit</);
});

test("a tabs folder that is a link or a file blocks every tab and names the folder, not a tab file", () => {
  let overview = base();
  for (const tab of ["quick_info", "features", "competitors", "install_and_use"]) overview = blockedTab(overview, tab, "not_a_folder");
  for (const tab of ["features", "competitors", "install_and_use"]) {
    const html = page({ selected: tab, overview });
    assert.match(html, /CrazyAgent can&#x27;t show this tab: the folder for the tabs is a link or a file\./, tab);
    assert.match(html, /<span class="pj-mono">\.crazyagent\/overview<\/span>/, tab);
    assert.match(html, /Remove it, then choose Refresh\./, tab);
    assert.doesNotMatch(html, /\.md<\/span>/, tab);
    assert.doesNotMatch(html, />Edit</, tab);
  }
});

test("a blocked Quick info keeps the About and the mapping's overview under the reason", () => {
  const html = page({ selected: "quick_info", about: "A research reader.", mappingOverview: "Reads papers.\n", overview: blockedTab(base(), "quick_info", "blank") });
  assert.match(html, /CrazyAgent can&#x27;t show this tab: its file is empty\./);
  assert.match(html, /\.crazyagent\/overview\/quick-info\.md/);
});

test("a blocked Competitors tab says why and does not offer Run research in its place", () => {
  const html = page({ selected: "competitors", overview: blockedTab({ ...base(), competitors: { kind: "report", path: "context/research/competitor_research.md" } }, "competitors", "too_large") });
  assert.match(html, new RegExp(`CrazyAgent can&#x27;t show this tab: its file is larger than ${view.TAB_BYTES_MAX / 1024} KB\\.`));
  assert.doesNotMatch(html, /Choose Refresh to write this tab from it\./);
});

test("a project outside the CrazyAgent folder says the tabs need it there and offers the move, on every generated tab", () => {
  const overview = { ...base(), needs_home: true, stale: true };
  for (const selected of TAB_IDS) {
    const html = page({ selected, overview });
    assert.match(html, /Overview tabs need this project in the CrazyAgent folder\./, selected);
    assert.match(html, /<button[^>]*>Move into CrazyAgent folder<\/button>/, selected);
    assert.doesNotMatch(html, /Not written yet/, selected);
    assert.doesNotMatch(html, />Run research</, selected);
    assert.match(html, /<button[^>]*disabled=""[^>]*>Refresh<\/button>/, selected);
  }
  assert.equal(view.pollEvery(overview), null, "no run can start, so nothing is waited for");
});

test("a project outside the CrazyAgent folder keeps README and Activity as they are", () => {
  const overview = { ...base(), needs_home: true, readme: { path: "README.md", text: "# Alpha\n\nReads papers.\n", cut: false } };
  const readme = page({ selected: "readme", overview });
  assert.match(readme, /Reads papers\./);
  assert.doesNotMatch(readme, /Overview tabs need this project/);
  const activity = page({ selected: "activity", overview });
  assert.match(activity, /Today&#x27;s activity/);
  assert.doesNotMatch(activity, /Overview tabs need this project/);
});

test("the move button opens Project settings, where Move into CrazyAgent folder lives", () => {
  assert.equal(view.moveRoute("alpha beta"), "/projects/alpha%20beta/settings?section=remove");
});

test("the needs-home refusal has its own sentence", () => {
  assert.equal(api.overviewCopy("overview_needs_project_home"), "Overview tabs need this project in the CrazyAgent folder. Move it there in Project settings.");
  assert.notEqual(api.overviewCopy("overview_needs_project_home"), api.overviewCopy("something_else"));
});

test("a shell block on a tab without commands shows whole, with no actions", () => {
  const html = page({ selected: "features", overview: withTab(base(), "features", "```sh\ncargo install alpha\nalpha init\n```\n"), runMode: "type_and_run" });
  assert.match(html, /<pre><code>cargo install alpha\nalpha init<\/code><\/pre>/);
  assert.doesNotMatch(html, />Copy</);
});

const settings = await import("../src/screens/project/settings.tsx");
const ok = (value) => ({ kind: "ok", value });

function settingsPage({ overview = {}, runMode = "type_and_run", read, mode, loading = false, note = "" } = {}) {
  return renderToStaticMarkup(createElement(settings.OverviewSettingsRows, {
    overview: loading ? undefined : read ?? ok({ ...base(), ...overview }), runMode: loading ? undefined : mode ?? ok(runMode),
    busy: false, note, onRefresh: () => {}, onApply: () => {}, onRunMode: () => {}, onMove: () => {},
  }));
}
const checked = (label) => new RegExp(`role="radio" aria-checked="true"[^>]*>${label}<`);

test("Project settings lists the three Overview settings with every option", () => {
  const html = settingsPage({ overview: { refresh: "after_landing", apply: "always_ask" }, runMode: "type_and_run" });
  for (const word of ["After each landed change set and Refresh", "Refresh only", "Off", "Always ask me", "Apply when I have not edited the tab", "Type and run", "Type only", "Off, Copy only"]) assert.match(html, new RegExp(`>${word}<`));
  for (const group of ["Overview tabs", "Refreshed tabs", "Run in project terminal"]) assert.match(html, new RegExp(`role="radiogroup" aria-label="${group}"`));
  assert.equal((html.match(/role="radio"/g) ?? []).length, 8);
  for (const label of ["After each landed change set and Refresh", "Always ask me", "Type and run"]) assert.match(html, checked(label));
  assert.equal((html.match(/aria-checked="true"/g) ?? []).length, 3);
});

test("each Overview setting shows the saved choice", () => {
  const html = settingsPage({ overview: { refresh: "refresh_only", apply: "when_unedited" }, runMode: "type_only" });
  for (const label of ["Refresh only", "Apply when I have not edited the tab", "Type only"]) assert.match(html, checked(label));
  const off = settingsPage({ overview: { refresh: "off" }, runMode: "off" });
  assert.match(off, checked("Off"));
  assert.match(off, checked("Off, Copy only"));
});

test("each Overview setting says what it does, what a refresh costs and the recommended choice", () => {
  const html = settingsPage();
  assert.match(html, /A refresh costs one agent run on this project&#x27;s pool\./);
  assert.match(html, /Recommended: After each landed change set and Refresh\./);
  assert.match(html, /Recommended: Always ask me\./);
  assert.match(html, /Recommended: Type and run\./);
  assert.match(html, /you confirm/);
});

test("the Overview settings of a project outside the CrazyAgent folder say the tabs need it there and offer the move", () => {
  const html = settingsPage({ overview: { needs_home: true } });
  assert.match(html, /Overview tabs need this project in the CrazyAgent folder\./);
  assert.match(html, /<button[^>]*>Move into CrazyAgent folder<\/button>/);
  assert.equal((html.match(/role="radio"/g) ?? []).length, 8, "the settings stay");
  assert.doesNotMatch(settingsPage(), /Overview tabs need this project/);
});

test("the Overview settings say Loading, and a failed read of one keeps the other", () => {
  assert.match(settingsPage({ loading: true }), /Loading\.\.\./);
  assert.doesNotMatch(settingsPage({ loading: true }), /role="radio"/);
  const lost = settingsPage({ read: { kind: "error", code: "overview_unavailable" } });
  assert.match(lost, /Couldn&#x27;t load the Overview settings\. The Overview tabs can&#x27;t be read right now\. Try again in a moment\./);
  assert.equal((lost.match(/role="radio"/g) ?? []).length, 3, "the Run setting still shows");
  const early = settingsPage({ read: { kind: "not_ready" }, mode: { kind: "not_ready" } });
  assert.match(early, /the Overview tabs aren&#x27;t in this build\./);
  assert.match(early, /Run in project terminal isn&#x27;t in this build\./);
  assert.doesNotMatch(early, /role="radio"/);
  const half = settingsPage({ mode: { kind: "error", code: "ide_unavailable" } });
  assert.match(half, /Couldn&#x27;t load the Run in project terminal setting\./);
  assert.equal((half.match(/role="radio"/g) ?? []).length, 5, "the Overview settings still show");
});

test("a refused Overview setting shows why", () => {
  assert.match(settingsPage({ note: "Overview tabs are off for this project." }), /role="status"[^>]*>Overview tabs are off for this project\./);
});

test("Project settings has an Overview section between General and Pool", () => {
  assert.equal(settings.settingsSection("overview"), "overview");
  const html = renderToStaticMarkup(createElement(settings.SettingsTab, { data: projectData(), section: "overview" }));
  assert.match(html, /<button[^>]*aria-current="page"[^>]*>Overview<\/button>/);
  assert.match(html, />General<\/button><button[^>]*>Overview<\/button><button[^>]*>Pool<\/button>/);
  assert.match(html, /<h2[^>]*>Overview<\/h2>/);
});

test("a generated tab offers Edit, written or not; README, a blocked tab and a project outside the folder do not", () => {
  const edit = /<button[^>]*>Edit<\/button>/;
  assert.match(page({ selected: "features", overview: withTab(base(), "features", "- Search\n") }), edit);
  assert.match(page({ selected: "features", overview: base() }), edit);
  assert.match(page({ selected: "install_and_use", overview: withTab(base(), "install_and_use", "Run it.\n") }), edit);
  assert.match(page({ selected: "competitors", overview: { ...withTab(base(), "competitors", "- Beta\n"), competitors: { kind: "report", path: "context/research/competitor_research.md" } } }), edit);
  assert.doesNotMatch(page({ selected: "features", overview: blockedTab(base(), "features", "blank") }), edit);
  assert.doesNotMatch(page({ selected: "features", overview: { ...base(), needs_home: true } }), edit);
  assert.doesNotMatch(page({ selected: "readme", overview: { ...base(), readme: { path: "README.md", text: "# Alpha\n", cut: false } } }), edit);
  assert.doesNotMatch(page({ selected: "activity", overview: base() }), edit);
});

function editor(props = {}) {
  return renderToStaticMarkup(createElement(view.TabEditor, { label: "Features", text: "- Search\n", busy: false, note: "", onText: () => {}, onSave: () => {}, onCancel: () => {}, ...props }));
}

test("the editor is a labelled text box with the tab's text, Save and Cancel", () => {
  const html = editor();
  assert.match(html, /<label[^>]*for="ov-edit"[^>]*>Edit Features<\/label>/);
  assert.match(html, /<textarea[^>]*id="ov-edit"[^>]*>- Search\n<\/textarea>/);
  assert.match(html, /<button[^>]*>Save<\/button>/);
  assert.match(html, /<button[^>]*>Cancel<\/button>/);
  assert.doesNotMatch(html, /role="alert"/);
});

test("the editor shows why a save was refused and locks while a save is sent", () => {
  assert.match(editor({ note: "This text is too long for a tab. Shorten it and save again." }), /role="alert"[^>]*>This text is too long for a tab\./);
  const busy = editor({ busy: true });
  assert.match(busy, /<button[^>]*disabled=""[^>]*>/);
  assert.match(busy, /<textarea[^>]*readOnly=""/i);
});

test("Save sends the text with the version the editor opened at", async () => {
  const sent = [];
  const outcome = await view.saveTab("alpha", { tab: "features", version: "v7" }, "- typed\n", (projectId, tab, text, version) => { sent.push([projectId, tab, text, version]); return Promise.resolve({ kind: "ok", value: undefined }); });
  assert.deepEqual(sent, [["alpha", "features", "- typed\n", "v7"]]);
  assert.deepEqual(outcome, { saved: true, changed: false, kept: "", note: "" });
});

const draftAt = (version, more = {}) => ({ version, text: "- typed", busy: false, note: "", changed: false, told: false, ...more });
const tabAt = (version) => ({ ...base().tabs.find((tab) => tab.tab === "features"), text: "- Search\n", version });

test("a tab that changed under an open editor holds the Save, says so and sends nothing; every later change holds it again", () => {
  const notice = api.overviewCopy("overview_tab_changed");
  assert.deepEqual(view.saveStep(draftAt("v1"), tabAt("v1")), { send: "v1" });
  const held = view.saveStep(draftAt("v1"), tabAt("v2"));
  assert.deepEqual(held, { hold: draftAt("v2", { note: notice, changed: true }) });
  assert.deepEqual(view.saveStep(held.hold, tabAt("v2")), { send: "v2" }, "the second Save is made against the version now shown");
  assert.deepEqual(view.saveStep({ ...held.hold, note: "" }, tabAt("v3")), { hold: draftAt("v3", { note: notice, changed: true }) }, "a further change holds the Save again");
  assert.throws(() => view.saveStep(draftAt("v1", { busy: true }), tabAt("v1")), /while another was still sent/);
});

test("after the engine said the tab changed, the next read is the tab the owner compares with, and a further change before Save shows the notice again and sends nothing", async () => {
  const outcome = await view.saveTab("alpha", { tab: "features", version: "v1" }, "- typed", () => Promise.resolve({ kind: "error", code: "overview_tab_changed" }));
  const told = view.afterSave(draftAt("v1", { busy: true }), outcome);
  assert.deepEqual(told, draftAt("v1", { note: api.overviewCopy("overview_tab_changed"), changed: true, told: true }));
  assert.equal(view.draftOnRead(told, tabAt("v1")), told, "the read that shows the change has not come yet");
  assert.deepEqual(view.saveStep(told, tabAt("v1")), { send: "v1" }, "until then a Save still names the version the engine refuses");
  const compared = view.draftOnRead(told, tabAt("v2"));
  assert.deepEqual(compared, draftAt("v2", { note: outcome.note, changed: true }));
  assert.equal(view.draftOnRead(compared, tabAt("v3")), compared, "a later read is not taken as compared");
  const held = view.saveStep(compared, tabAt("v3"));
  assert.deepEqual(held, { hold: draftAt("v3", { note: api.overviewCopy("overview_tab_changed"), changed: true }) });
  assert.deepEqual(view.saveStep(held.hold, tabAt("v3")), { send: "v3" });
});

test("a saved editor closes, and a save refused for another reason keeps the editor as it was with the reason", () => {
  assert.equal(view.afterSave(draftAt("v1", { busy: true }), { saved: true, changed: false, kept: "", note: "" }), null);
  const refused = { saved: false, changed: false, kept: "- typed", note: "No." };
  assert.deepEqual(view.afterSave(draftAt("v1", { busy: true }), refused), draftAt("v1", { note: "No." }));
  assert.deepEqual(view.afterSave(draftAt("v1", { busy: true, changed: true, told: true }), refused), draftAt("v1", { note: "No.", changed: true, told: true }), "an editor waiting for the changed tab keeps waiting");
  assert.throws(() => view.afterSave(draftAt("v1"), { ...refused, note: "" }), /says why/);
});

test("an open editor's text is kept per project and tab for the session, and a saved or cancelled one leaves", () => {
  const stored = new Map();
  const store = { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  assert.deepEqual(view.storedDrafts("alpha", store), {});
  view.storeDrafts("alpha", { features: draftAt("v1", { busy: true, note: "No.", changed: true, told: true }), competitors: draftAt("v4", { text: "- Beta" }) }, store);
  assert.deepEqual(view.storedDrafts("alpha", store), { features: draftAt("v1", { changed: true }), competitors: draftAt("v4", { text: "- Beta" }) }, "kept unlocked, with no note, at the version last compared");
  assert.deepEqual(view.storedDrafts("beta", store), {}, "another project has its own editors");
  view.storeDrafts("alpha", { features: undefined, competitors: draftAt("v4", { text: "- Beta" }) }, store);
  assert.deepEqual(view.storedDrafts("alpha", store), { competitors: draftAt("v4", { text: "- Beta" }) });
  view.storeDrafts("alpha", {}, store);
  assert.deepEqual(view.storedDrafts("alpha", store), {});
});

test("a kept editor that is no editor, is too long or sits in a storage that refuses reads as none", () => {
  const stored = new Map();
  const store = { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  view.storeDrafts("alpha", { features: draftAt("v1") }, store);
  const [key] = [...stored.keys()];
  for (const kept of ["nonsense", "null", "7", "[]", JSON.stringify({ features: null }), JSON.stringify({ features: { version: 7, text: "- typed", changed: false } }),
    JSON.stringify({ features: { version: "v1", text: ["- typed"], changed: false } }), JSON.stringify({ features: { version: "v1", text: "- typed" } }),
    JSON.stringify({ readme: { version: "v1", text: "- typed", changed: false } }), JSON.stringify({ features: { version: "v1", text: "x".repeat(view.DRAFT_CHARS_MAX + 1), changed: false } })]) {
    stored.set(key, kept);
    assert.deepEqual(view.storedDrafts("alpha", store), {}, kept.slice(0, 60));
  }
  stored.set(key, JSON.stringify({ features: { version: "v1", text: "x".repeat(view.DRAFT_CHARS_MAX), changed: false, busy: true, note: "No.", told: true } }));
  assert.deepEqual(view.storedDrafts("alpha", store), { features: draftAt("v1", { text: "x".repeat(view.DRAFT_CHARS_MAX) }) });
  view.storeDrafts("alpha", { features: draftAt("v1", { text: "x".repeat(view.DRAFT_CHARS_MAX + 1) }) }, store);
  assert.deepEqual(view.storedDrafts("alpha", store), {}, "a text over the bound is not kept");
  const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
  assert.deepEqual(view.storedDrafts("alpha", broken), {});
  assert.doesNotThrow(() => view.storeDrafts("alpha", { features: draftAt("v1") }, broken));
});

test("coming back to the Overview reopens a kept editor with the typed text, and shows the tab as it is now once it changed", () => {
  const stored = new Map();
  const store = { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  const overview = withTab(base(), "features", "- Search\n");
  assert.doesNotMatch(page({ selected: "features", overview, store }), /<textarea/);
  view.storeDrafts("alpha", { features: draftAt("v-features", { text: "- typed and not saved" }) }, store);
  const back = page({ selected: "features", overview, store });
  assert.match(back, /<label[^>]*for="ov-edit"[^>]*>Edit Features<\/label>/);
  assert.match(back, /<textarea[^>]*id="ov-edit"[^>]*>- typed and not saved<\/textarea>/);
  assert.doesNotMatch(back, /The tab as it is now:/);
  assert.doesNotMatch(page({ selected: "install_and_use", overview, store }), /<textarea/, "only the tab it was typed in");
  view.storeDrafts("alpha", { features: draftAt("v0", { text: "- typed and not saved", changed: true }) }, store);
  const changed = page({ selected: "features", overview, store });
  assert.match(changed, /The tab as it is now:/);
  assert.match(changed, /<textarea[^>]*id="ov-edit"[^>]*>- typed and not saved<\/textarea>/);
});

test("a tab changed since the editor opened keeps the typed text and says to compare", async () => {
  const outcome = await view.saveTab("alpha", { tab: "features", version: "v7" }, "- typed\n", () => Promise.resolve({ kind: "error", code: "overview_tab_changed" }));
  assert.equal(outcome.saved, false);
  assert.equal(outcome.changed, true);
  assert.equal(outcome.kept, "- typed\n");
  assert.match(outcome.note, /Someone changed this tab since you opened it\./);
});

test("any other refused save keeps the typed text and says why", async () => {
  const blocked = await view.saveTab("alpha", { tab: "features", version: "v7" }, "- typed\n", () => Promise.resolve({ kind: "error", code: "overview_tab_blocked" }));
  assert.deepEqual([blocked.saved, blocked.changed, blocked.kept], [false, false, "- typed\n"]);
  assert.equal(blocked.note, api.overviewCopy("overview_tab_blocked"));
  const early = await view.saveTab("alpha", { tab: "features", version: "v7" }, "- typed\n", () => Promise.resolve({ kind: "not_ready" }));
  assert.deepEqual([early.saved, early.kept], [false, "- typed\n"]);
  assert.notEqual(early.note, "");
  const thrown = await view.saveTab("alpha", { tab: "features", version: "v7" }, "- typed\n", () => Promise.reject(new Error("bridge down")));
  assert.deepEqual([thrown.saved, thrown.kept], [false, "- typed\n"]);
  assert.notEqual(thrown.note, "");
});

test("the tab bound is the engine's, in whole KB, and the limit an owner reads follows it", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/project_overview.rs", import.meta.url), "utf8");
  const bound = /^pub const TAB_BYTES_MAX: usize = (\d+) \* 1024;$/m.exec(engine);
  assert.ok(bound, "the engine names its tab bound in KiB");
  assert.equal(view.TAB_BYTES_MAX, Number(bound[1]) * 1024);
  assert.equal(view.TAB_BYTES_MAX % 1024, 0);
  const html = page({ selected: "features", overview: blockedTab(base(), "features", "too_large") });
  assert.ok(html.includes(`its file is larger than ${bound[1]} KB.`), "the blocked line names the engine's limit");
});

test("the editor refuses a text over the tab bound or an empty one before sending", async () => {
  let sent = 0;
  const save = () => { sent += 1; return Promise.resolve({ kind: "ok", value: undefined }); };
  const long = await view.saveTab("alpha", { tab: "features", version: "v7" }, "x".repeat(view.TAB_BYTES_MAX + 1), save);
  assert.equal(long.saved, false);
  assert.match(long.note, /too long/);
  const wide = await view.saveTab("alpha", { tab: "features", version: "v7" }, String.fromCodePoint(0x1f600).repeat(view.TAB_BYTES_MAX / 4 + 1), save);
  assert.match(wide.note, /too long/, "the bound counts UTF-8 bytes, as the engine does");
  const empty = await view.saveTab("alpha", { tab: "features", version: "v7" }, " \n\t\n", save);
  assert.equal(empty.note, api.overviewCopy("overview_tab_empty"));
  assert.equal(sent, 0);
  const full = await view.saveTab("alpha", { tab: "features", version: "v7" }, "x".repeat(view.TAB_BYTES_MAX), save);
  assert.equal(full.saved, true);
  assert.equal(sent, 1);
});

const withProposal = (overview, id, proposal, edited = false, seal = "p1") => ({ ...overview, tabs: overview.tabs.map((tab) => tab.tab === id ? { ...tab, proposal, edited, proposal_version: seal } : tab) });
const featuresOf = (overview) => overview.tabs.find((tab) => tab.tab === "features");

function review(state, props = {}) {
  return renderToStaticMarkup(createElement(view.ProposalReview, { label: "Features", state, busy: false, note: "", onAccept: () => {}, onReject: () => {}, onClose: () => {}, ...props }));
}

test("a tab with a proposal shows a banner with Review, and a tab without one does not", () => {
  const overview = withProposal(withTab(base(), "features", "- Search\n"), "features", "- Search\n- Share lists\n");
  const html = page({ selected: "features", overview });
  assert.match(html, /A refresh proposes changes to this tab\./);
  assert.match(html, /<button[^>]*>Review<\/button>/);
  assert.match(html, /Search/, "the tab's own text stays until Accept");
  assert.doesNotMatch(html, /Share lists/, "the proposal is not shown as the tab");
  assert.doesNotMatch(page({ selected: "features", overview: withTab(base(), "features", "- Search\n") }), /A refresh proposes changes/);
  assert.doesNotMatch(page({ selected: "install_and_use", overview }), /A refresh proposes changes/, "only the tab with the proposal");
  assert.doesNotMatch(page({ selected: "features", overview: blockedTab(overview, "features", "too_large") }), /A refresh proposes changes/, "a blocked tab cannot take a proposal");
});

test("the review shows the diff with Accept and Reject, and nothing changes until Accept", () => {
  const state = featuresOf(withProposal(withTab(base(), "features", "- Search\n- Old\n"), "features", "- Search\n- Share lists\n"));
  const html = review(state);
  assert.match(html, /<ins[^>]*class="pr-diff-row added"[^>]*><span class="visually-hidden">Added: <\/span>- Share lists<\/ins>/);
  assert.match(html, /<del[^>]*class="pr-diff-row removed"[^>]*><span class="visually-hidden">Removed: <\/span>- Old<\/del>/);
  assert.match(html, /<span class="pr-diff-row">- Search<\/span>/);
  assert.match(html, /1 line removed, 1 added\. Nothing changes until you accept\./);
  assert.match(html, /<button[^>]*>Accept<\/button>/);
  assert.match(html, /<button[^>]*>Reject<\/button>/);
  assert.match(html, /<button[^>]*>Close<\/button>/);
  assert.doesNotMatch(html, /You edited this tab/);
});

test("the review of an edited tab says the owner's edits are kept until Accept", () => {
  const state = featuresOf(withProposal(withTab(base(), "features", "- mine\n"), "features", "- Search\n", true));
  assert.match(review(state), /You edited this tab\. Accept replaces your text with the proposal; Reject keeps yours\./);
});

test("a proposal too long to mark line by line shows both texts whole", () => {
  const long = "line\n".repeat(2001);
  const html = review(featuresOf(withProposal(withTab(base(), "features", "- Search\n"), "features", long)));
  assert.match(html, /Too many lines to mark each change, so both texts are shown whole\./);
  assert.match(html, />The tab now</);
  assert.match(html, />Proposed</);
  assert.doesNotMatch(html, /<ins/);
});

test("the review shows hidden characters of a proposal as visible escapes, locks while sending and says why a choice was refused", () => {
  const sneaky = `- Share${String.fromCodePoint(0x202e)} lists\n`;
  const html = review(featuresOf(withProposal(withTab(base(), "features", "- Search\n"), "features", sneaky)));
  assert.ok(!html.includes(String.fromCodePoint(0x202e)));
  assert.ok(html.includes(`${String.fromCharCode(92)}u202e`));
  const state = featuresOf(withProposal(withTab(base(), "features", "- Search\n"), "features", "- New\n"));
  assert.equal((review(state, { busy: true }).match(/<button[^>]*disabled=""/g) ?? []).length, 3);
  assert.match(review(state, { note: "There is no proposal for this tab any more." }), /role="alert"[^>]*>There is no proposal for this tab any more\./);
});

test("Accept sends the tab version and the proposal version the diff was made from", async () => {
  const sent = [];
  const outcome = await view.acceptTab("alpha", { tab: "features", version: "v7", proposal_version: "p3" }, (projectId, tab, version, seal) => { sent.push([projectId, tab, version, seal]); return Promise.resolve({ kind: "ok", value: undefined }); });
  assert.deepEqual(sent, [["alpha", "features", "v7", "p3"]]);
  assert.deepEqual(outcome, { done: true, note: "" });
});

test("an Accept on a tab that changed since the diff says the diff is new, and other refusals say why", async () => {
  const changed = await view.acceptTab("alpha", { tab: "features", version: "v7", proposal_version: "p3" }, () => Promise.resolve({ kind: "error", code: "overview_tab_changed" }));
  assert.equal(changed.done, false);
  assert.match(changed.note, /This tab changed since this diff was made\./);
  const gone = await view.acceptTab("alpha", { tab: "features", version: "v7", proposal_version: "p3" }, () => Promise.resolve({ kind: "error", code: "overview_no_proposal" }));
  assert.equal(gone.note, api.overviewCopy("overview_no_proposal"));
  const early = await view.acceptTab("alpha", { tab: "features", version: "v7", proposal_version: "p3" }, () => Promise.resolve({ kind: "not_ready" }));
  assert.equal(early.done, false);
  assert.notEqual(early.note, "");
  const thrown = await view.acceptTab("alpha", { tab: "features", version: "v7", proposal_version: "p3" }, () => Promise.reject(new Error("bridge down")));
  assert.equal(thrown.done, false);
});

test("Reject names the tab and keeps the tab's text", async () => {
  const sent = [];
  const outcome = await view.rejectTab("alpha", "features", (projectId, tab) => { sent.push([projectId, tab]); return Promise.resolve({ kind: "ok", value: undefined }); });
  assert.deepEqual(sent, [["alpha", "features"]]);
  assert.deepEqual(outcome, { done: true, note: "" });
  const refused = await view.rejectTab("alpha", "features", () => Promise.resolve({ kind: "error", code: "overview_no_proposal" }));
  assert.deepEqual(refused, { done: false, note: api.overviewCopy("overview_no_proposal") });
});

const reviewAt = (seal, more = {}, version = "v-features") => ({ busy: false, note: "", seen: { version, proposal_version: seal }, told: false, ...more });
const proposed = (seal) => featuresOf(withProposal(withTab(base(), "features", "- Search\n"), "features", "- New\n", false, seal));

test("an Accept is sent only for the diff the owner was shown; a newer proposal is shown first and needs a second Accept, every time", () => {
  const notice = api.overviewCopy("overview_proposal_changed");
  assert.deepEqual(view.acceptStep(reviewAt("p1"), proposed("p1")), { send: { version: "v-features", proposal_version: "p1" } });
  const held = view.acceptStep(reviewAt("p1"), proposed("p2"));
  assert.deepEqual(held, { hold: reviewAt("p2", { note: notice }) });
  assert.deepEqual(view.acceptStep(held.hold, proposed("p2")), { send: { version: "v-features", proposal_version: "p2" } }, "the second Accept sends the proposal now shown");
  assert.deepEqual(view.acceptStep({ ...held.hold, note: "" }, proposed("p3")), { hold: reviewAt("p3", { note: notice }) }, "a further proposal holds the Accept again");
  assert.throws(() => view.acceptStep(reviewAt("p1", { busy: true }), proposed("p1")), /while another choice was still sent/);
});

test("a tab that changed under an open review holds the Accept and says the diff is new", () => {
  const moved = { ...proposed("p1"), version: "v8", text: "- Other\n" };
  const held = view.acceptStep(view.openReview(proposed("p1")), moved);
  assert.deepEqual(held, { hold: reviewAt("p1", { note: "This tab changed since this diff was made. The diff now shows the change against the tab as it is; choose again." }, "v8") });
  assert.deepEqual(view.acceptStep(held.hold, moved), { send: { version: "v8", proposal_version: "p1" } });
  const both = view.acceptStep(view.openReview(proposed("p1")), { ...moved, proposal_version: "p2" });
  assert.deepEqual(both, { hold: reviewAt("p2", { note: api.overviewCopy("overview_proposal_changed") }, "v8") }, "a newer proposal on a changed tab is one hold on the diff shown");
});

test("after the engine said a proposal was replaced, the next read is the one the owner was told about, and a later one holds the Accept again", async () => {
  const notice = api.overviewCopy("overview_proposal_changed");
  const refused = await view.acceptTab("alpha", { tab: "features", version: "v-features", proposal_version: "p1" }, () => Promise.resolve({ kind: "error", code: "overview_proposal_changed" }));
  assert.deepEqual(refused, { done: false, note: notice, stale: true });
  assert.equal(notice, "A newer proposal replaced the one you opened. The diff now shows it; look it over and choose again.");
  const told = view.afterChoice(reviewAt("p1", { busy: true }), refused);
  assert.deepEqual(told, reviewAt("p1", { note: notice, told: true }));
  assert.equal(view.reviewOnRead(told, proposed("p1")), told, "the read that shows the newer proposal has not come yet");
  const first = view.reviewOnRead(told, proposed("p2"));
  assert.deepEqual(first, reviewAt("p2", { note: notice }));
  assert.equal(view.reviewOnRead(first, proposed("p3")), first, "a later read is not taken as seen");
  const held = view.acceptStep(first, proposed("p3"));
  assert.deepEqual(held, { hold: reviewAt("p3", { note: notice }) }, "the second Accept is held and says a newer proposal is shown");
  assert.deepEqual(view.acceptStep(held.hold, proposed("p3")), { send: { version: "v-features", proposal_version: "p3" } }, "the third sends the last version");
});

test("after the engine said the tab changed under a diff, the next read is the diff the owner was told about, and a later change holds the Accept again", async () => {
  const refused = await view.acceptTab("alpha", { tab: "features", version: "v-features", proposal_version: "p1" }, () => Promise.resolve({ kind: "error", code: "overview_tab_changed" }));
  assert.equal(refused.stale, true);
  const told = view.afterChoice(reviewAt("p1", { busy: true }), refused);
  assert.deepEqual(told, reviewAt("p1", { note: refused.note, told: true }));
  const first = view.reviewOnRead(told, { ...proposed("p1"), version: "v8" });
  assert.deepEqual(first, reviewAt("p1", { note: refused.note }, "v8"));
  const held = view.acceptStep(first, { ...proposed("p1"), version: "v9" });
  assert.deepEqual(held, { hold: reviewAt("p1", { note: refused.note }, "v9") });
  assert.deepEqual(view.acceptStep(held.hold, { ...proposed("p1"), version: "v9" }), { send: { version: "v9", proposal_version: "p1" } });
});

test("a choice refused for another reason keeps the review on its diff with the reason, and a done one closes it", () => {
  assert.deepEqual(view.afterChoice(reviewAt("p1", { busy: true }), { done: false, note: "No." }), reviewAt("p1", { note: "No." }));
  assert.deepEqual(view.afterChoice(reviewAt("p1", { busy: true, told: true }), { done: false, note: "No." }), reviewAt("p1", { note: "No.", told: true }), "a review waiting for the new diff keeps waiting");
  assert.equal(view.afterChoice(reviewAt("p1", { busy: true }), { done: true, note: "" }), null);
  assert.throws(() => view.afterChoice(reviewAt("p1", { busy: true }), { done: false, note: "" }), /says why/);
});

test("Review opens on the tab version and the proposal version the banner was shown for", () => {
  assert.deepEqual(view.openReview(proposed("p4")), reviewAt("p4"));
  assert.throws(() => view.openReview(featuresOf(withTab(base(), "features", "- Search\n"))), /no proposal/);
});

test("opening the editor puts the keyboard in its text box, and the review in its panel", () => {
  assert.match(editor(), /<textarea[^>]*autofocus=""/i);
  const state = featuresOf(withProposal(withTab(base(), "features", "- Search\n"), "features", "- New\n"));
  assert.match(review(state), /<section[^>]*id="ov-review"[^>]*tabindex="-1"/i);
});

test("the keyboard goes back to Edit after Save or Cancel, and to Review or Edit after a review closes", () => {
  const idle = (proposal = false) => ({ mode: "idle", proposal });
  const editing = (proposal = false) => ({ mode: "editing", proposal });
  const reviewing = { mode: "reviewing", proposal: true };
  assert.equal(view.focusAfter(editing(), idle()), "ov-edit-open", "Save or Cancel returns to Edit");
  assert.equal(view.focusAfter(editing(true), idle(true)), "ov-edit-open");
  assert.equal(view.focusAfter(idle(true), reviewing), "ov-review", "Review moves into the panel");
  assert.equal(view.focusAfter(reviewing, idle(true)), "ov-review-open", "Close returns to Review");
  assert.equal(view.focusAfter(reviewing, idle(false)), "ov-edit-open", "after Accept or Reject the banner is gone, so Edit takes it");
  assert.equal(view.focusAfter(idle(true), idle(false)), "ov-edit-open", "the banner the keyboard was on left with the proposal");
  assert.equal(view.focusAfter(idle(), editing()), null, "the text box takes the keyboard itself");
  assert.equal(view.focusAfter(idle(), idle()), null, "nothing moves on a plain read");
  assert.equal(view.focusAfter(idle(false), idle(true)), null, "a new proposal does not take the keyboard");
  assert.equal(view.focusAfter(reviewing, reviewing), null);
  assert.equal(view.focusAfter(editing(), editing()), null);
});

test("Edit and Review carry the ids the keyboard returns to", () => {
  const overview = withProposal(withTab(base(), "features", "- Search\n"), "features", "- New\n");
  const html = page({ selected: "features", overview });
  assert.match(html, /<button[^>]*id="ov-edit-open"[^>]*>Edit<\/button>/);
  assert.match(html, /<button[^>]*id="ov-review-open"[^>]*>Review<\/button>/);
});
