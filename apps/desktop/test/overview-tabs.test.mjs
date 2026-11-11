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
  readme: null, tabs: TAB_IDS.map((tab) => ({ tab, text: null, version: "", edited: false, proposal: null, blocked: null })),
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

function page({ selected, overview, about = "", mappingOverview = null, researchRunning = false, jobs, research, runMode = "off", problem }) {
  const data = projectData(jobs ?? (researchRunning ? [job("competitor_research", "running")] : []), research ?? (researchRunning || jobs ? ["market_research", "competitor_research"] : ["market_research"]));
  return renderToStaticMarkup(createElement(view.OverviewTabs, {
    data, overview, problem, now: NOW, selected, onSelect: () => {}, actions, runMode,
    quickInfo: (text) => createElement(screen.OverviewFacts, { about, overview: text ?? mappingOverview }),
    activity: createElement("p", null, "Today's activity"),
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
  const codes = ["overview_tab_changed", "overview_tab_blocked", "overview_tab_too_large", "overview_tab_empty", "overview_no_proposal", "overview_off", "overview_project_unknown", "overview_unavailable"];
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
  assert.match(page({ selected: "features", overview: blockedTab(base(), "features", "too_large") }), /CrazyAgent can&#x27;t show this tab: its file is larger than 64 KB\./);
  assert.match(page({ selected: "install_and_use", overview: blockedTab(base(), "install_and_use", "not_utf8") }), /CrazyAgent can&#x27;t show this tab: its file is not plain text\./);
  assert.match(page({ selected: "install_and_use", overview: blockedTab(base(), "install_and_use", "not_utf8") }), /\.crazyagent\/overview\/install-and-use\.md/);
  const unknown = page({ selected: "features", overview: blockedTab(base(), "features", "something_new") });
  assert.match(unknown, /CrazyAgent can&#x27;t show this tab: its file can&#x27;t be read\./);
  assert.doesNotMatch(unknown, /something_new/);
  assert.doesNotMatch(blank, /Not written yet/);
});

test("a blocked Quick info keeps the About and the mapping's overview under the reason", () => {
  const html = page({ selected: "quick_info", about: "A research reader.", mappingOverview: "Reads papers.\n", overview: blockedTab(base(), "quick_info", "blank") });
  assert.match(html, /CrazyAgent can&#x27;t show this tab: its file is empty\./);
  assert.match(html, /\.crazyagent\/overview\/quick-info\.md/);
});

test("a blocked Competitors tab says why and does not offer Run research in its place", () => {
  const html = page({ selected: "competitors", overview: blockedTab({ ...base(), competitors: { kind: "report", path: "context/research/competitor_research.md" } }, "competitors", "too_large") });
  assert.match(html, /CrazyAgent can&#x27;t show this tab: its file is larger than 64 KB\./);
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
