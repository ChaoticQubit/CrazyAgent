import assert from "node:assert/strict";
import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, bridgeCall, value } from "./e2e-harness.mjs";
import { answering, handOff, onEngine } from "./codex-task-engine.mjs";

const RUN_MS = 180000;
const QUIET_MS = 10 * 60 * 1000;
const MARKER = "JUICE";
const QUICK = "Fruit stand sells fruit from a small shop.";
const FEATURE = "- Sells fruit";
const ADDED = "- Sells juice";
const RIVAL = "- Berry Barn: cheaper, no delivery.";
const INSTALL = "cargo install fruit-stand";
const OWNER_LINE = "- Written by the owner";
const OUTSIDE = "Ships pears by post";
const TABS = ["quick_info", "features", "competitors", "install_and_use"];

/** The reply of one writing run: the four tabs in the fenced form the engine reads, with one
 * more feature when asked. Input: whether the feature is added. Output: the reply. */
function reply(more) {
  return [
    "~~~quick-info", QUICK, "~~~",
    "~~~features", FEATURE, ...(more ? [ADDED] : []), "~~~",
    "~~~install-and-use", "## Install", "", "```sh", INSTALL, "```", "~~~",
    "~~~competitors", RIVAL, "~~~",
  ].join("\n");
}

/** The Codex stand-in for the writing runs: it answers with the plain tabs, and with one
 * more feature once the prompt it reads names the marker, which the test puts in the README.
 * Output: the script. */
function codex() {
  const quote = (text) => text.replaceAll("'", String.raw`'\''`);
  const base = answering(reply(false));
  const read = "  while IFS= read -r line; do :; done\n";
  const write = `  printf '%s\\n' '${quote(reply(false))}' > "$out" || exit 1\n`;
  assert.ok(base.includes(read) && base.includes(write), "the stand-in reads the prompt and writes one reply");
  const seen = `  seen=0\n  while IFS= read -r line; do case "$line" in *${MARKER}*) seen=1;; esac; done\n`;
  const pick = `  if [ "$seen" = 1 ]; then printf '%s\\n' '${quote(reply(true))}' > "$out" || exit 1; else printf '%s\\n' '${quote(reply(false))}' > "$out" || exit 1; fi\n`;
  return base.replace(read, seen).replace(write, pick);
}

const CODEX = codex();

/** The stand-in for the landed change set case: as CODEX, but a team sub-agent's turn, run
 * in its own worktree (<project>/.crazyagent/worktrees/agent-<id>), also writes fruit.md as
 * the handed-off task asks and commits it on its branch, as an agent in its own clone must,
 * since only its commits land; the commit's outcome goes to $CODEX_HOME/wrote. The master,
 * the owner's folder and the writing runs write nothing. Output: the script. */
function landingCodex() {
  const started = String.raw`  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"` + "\n";
  assert.equal(CODEX.split(started).length, 2, "the stand-in marks each turn started once");
  const write = String.raw`  case "$(/bin/pwd -P)" in */worktrees/agent-[0-9]*) printf '%s\n' 'Apples are crisp.' > fruit.md || exit 1; git add fruit.md && GIT_AUTHOR_NAME=Agent GIT_AUTHOR_EMAIL=agent@example.invalid GIT_COMMITTER_NAME=Agent GIT_COMMITTER_EMAIL=agent@example.invalid git commit -q -m 'Write fruit.md' > "$CODEX_HOME/wrote" 2>&1; printf 'exit %s\n' "$?" >> "$CODEX_HOME/wrote" ;; esac` + "\n";
  return CODEX.replace(started, write + started);
}

const LANDING_CODEX = landingCodex();

/** Reads a project's Overview through the bridge, the read the page makes. Inputs: the
 * bridge and the project. Output: the overview. */
function overview(bridge, project) {
  return value(bridge, "desktop_overview_read", { projectId: project });
}

/** One tab of an overview. Inputs: the overview and the tab's name. Output: its state. */
function tabOf(read, name) {
  const state = read.tabs.find((tab) => tab.tab === name);
  assert.ok(state, `the overview lists ${name}: ${JSON.stringify(read.tabs.map((tab) => tab.tab))}`);
  return state;
}

/** Waits until a project's Overview passes a check. Inputs: the bridge, the project, what is
 * waited for, the check and the wait. Output: the overview that passed. */
async function until(bridge, project, what, check, ms = RUN_MS) {
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS * 5)) {
    const read = await overview(bridge, project);
    if (check(read)) return read;
    assert.ok(Date.now() < deadline, `${what}: ${JSON.stringify({ ...read, readme: read.readme?.path ?? null })}`);
  }
}

/** Holds for a while and checks that no writing run started and no tab was written.
 * Inputs: the bridge, the project and how long. Output: none. */
async function staysUnwritten(bridge, project, ms) {
  for (const deadline = Date.now() + ms; Date.now() < deadline; await pause(POLL_MS * 10)) {
    const read = await overview(bridge, project);
    assert.equal(read.running, false, "no writing run started");
    for (const name of TABS) assert.equal(tabOf(read, name).text, null, `${name} stays unwritten`);
  }
}

/** Sends one Overview change through the bridge and returns its refusal code, or null when
 * the engine took it. Inputs: the bridge, the command and its arguments. Output: the code. */
async function refusal(bridge, command, args) {
  const answer = await bridgeCall(bridge, command, args);
  if ("value" in answer) return null;
  assert.equal(typeof answer.failure?.code, "string", `${command} answered ${JSON.stringify(answer)}`);
  return answer.failure.code;
}

/** The folder of a project's repository, where its README and .crazyagent folder are.
 * Inputs: the bridge and the project. Output: the absolute path. */
async function repoOf(bridge, project) {
  const folder = (await value(bridge, "desktop_project_setup", { id: project })).folder;
  return folder.endsWith("/.crazyagent") ? dirname(folder) : folder;
}

/** Opens a project's Overview and picks one of its tabs. Inputs: the page, the served app,
 * the owner, the project and the tab's label. Output: none. */
async function openTab(page, site, me, project, label) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/overview` });
  await me.waitFor("the Overview tab strip", (seen) => seen.includes("Quick info") && seen.includes("Install & use") && seen.includes("README"));
  await me.click(label, "[role=tab]");
}

/** Says whether a button with this exact label is on the page and whether it is disabled.
 * Inputs: the page and the label. Output: null when it is not there, else its disabled state. */
function button(page, label) {
  return page.evaluate(`(() => {
    const found = [...document.querySelectorAll("button")].find((item) => (item.innerText ?? "").trim() === ${JSON.stringify(label)});
    return found ? { disabled: found.disabled === true } : null;
  })()`);
}

/** Waits for the first writing run to fill the four tabs. Inputs: the bridge and the
 * project. Output: the overview with the tabs written. */
function written(bridge, project) {
  return until(bridge, project, "the first run never wrote the tabs", (read) => !read.running && TABS.every((name) => tabOf(read, name).text !== null || name === "competitors"));
}

test("Overview tabs, After each landed change set and Refresh with Always ask me: the first open writes the tabs, a refresh proposes and Accept applies", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-default-", async ({ project, me, page, site, bridge }) => {
    await openTab(page, site, me, project, "Features");
    const first = await written(bridge, project);
    assert.equal(first.refresh, "after_landing", "a project the owner never changed writes after landings and on Refresh");
    assert.equal(first.apply, "always_ask");
    assert.equal(tabOf(first, "features").text.trim(), FEATURE);
    assert.equal(tabOf(first, "quick_info").text.trim(), QUICK);
    assert.ok(tabOf(first, "install_and_use").text.includes(INSTALL));
    assert.ok(first.readme !== null && first.readme.text.includes("Sells fruit."), "the README tab holds the repository's own file");
    await me.waitFor("the Features tab's text", (seen) => seen.includes("Sells fruit") && /Checked /.test(seen));
    await me.capture("first-write");

    appendFileSync(join(await repoOf(bridge, project), "README.md"), `\nNow with ${MARKER}.\n`);
    await me.click("Refresh", "button");
    const proposed = await until(bridge, project, "the refresh never proposed", (read) => tabOf(read, "features").proposal !== null);
    assert.equal(tabOf(proposed, "features").text.trim(), FEATURE, "Always ask me changes nothing by itself");
    assert.ok(tabOf(proposed, "features").proposal.includes(ADDED));
    await me.waitFor("the proposal banner", (seen) => seen.includes("A refresh proposes changes to this tab."));
    await me.click("Review", "button");
    const review = await me.waitFor("the diff", (seen) => seen.includes("Nothing changes until you accept."));
    assert.ok(review.includes("0 lines removed, 1 added."), `the change count: ${review.slice(0, 1500)}`);
    await me.capture("proposal-diff");
    await me.click("Accept", "button");
    const accepted = await until(bridge, project, "Accept never applied", (read) => tabOf(read, "features").proposal === null && tabOf(read, "features").text.includes(ADDED), STEP_MS);
    assert.equal(tabOf(accepted, "features").edited, false);
    await me.waitFor("the accepted tab", (seen) => seen.includes("Sells juice") && !seen.includes("A refresh proposes changes to this tab."));
    await me.capture("accepted");
  }, 1, CODEX);
});

test("Overview tabs, After each landed change set: a landed change set brings a proposal after the quiet period and one run", { timeout: 2400000 }, async (t) => {
  await onEngine(t, "ca-overview-landed-", async ({ project, runtime, me, page, site, bridge }) => {
    const home = join(dirname(runtime), "home");
    mkdirSync(home, { recursive: true, mode: 0o700 });
    writeFileSync(join(home, ".gitconfig"), "[user]\n\tname = E2E Owner\n\temail = owner@example.invalid\n");
    await openTab(page, site, me, project, "Features");
    await written(bridge, project);
    appendFileSync(join(await repoOf(bridge, project), "README.md"), `\nNow with ${MARKER}.\n`);
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
    assert.equal(await handOff(me, page, project, runtime), "done", "the sub-agent finished its task");
    const agents = join(runtime, "team", "agents");
    const wrote = readdirSync(agents).map((agent) => join(agents, agent, "home", "wrote")).filter((trace) => existsSync(trace)).map((trace) => readFileSync(trace, "utf8"));
    assert.deepEqual(wrote.map((trace) => trace.trim().split("\n").at(-1)), ["exit 0"], `the sub-agent committed fruit.md on its branch: ${JSON.stringify(wrote)}`);
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/changes` });
    await me.waitFor("the agent's change set", (seen) => /Ready to review/.test(seen));
    await me.click("Accept");
    await me.waitFor("the accept question", (seen) => /Accept these changes\?/.test(seen));
    await me.click("Accept", "dialog button");
    await me.waitFor("the landed note", (seen) => /Accepted as [0-9a-f]{7}/.test(seen), RUN_MS);
    const landedAt = Date.now();
    const waiting = await overview(bridge, project);
    assert.equal(waiting.stale, true, "a landed change set marks the tabs due");
    assert.equal(tabOf(waiting, "features").proposal, null, "nothing is proposed inside the quiet period");
    const proposed = await until(bridge, project, "the landing never brought a proposal", (read) => tabOf(read, "features").proposal !== null, QUIET_MS + RUN_MS);
    assert.ok(Date.now() - landedAt >= QUIET_MS, "the run waited for the quiet period");
    assert.ok(tabOf(proposed, "features").proposal.includes(ADDED));
  }, 1, LANDING_CODEX);
});

test("Overview tabs, Refresh only: nothing is written until the owner chooses Refresh", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-refresh-only-", async ({ project, me, page, site, bridge }) => {
    assert.equal(await refusal(bridge, "desktop_overview_set_refresh", { projectId: project, refresh: "refresh_only" }), null);
    await openTab(page, site, me, project, "Features");
    await me.waitFor("the unwritten line", (seen) => seen.includes("Not written yet. Choose Refresh to write it."));
    await staysUnwritten(bridge, project, 20000);
    assert.equal((await overview(bridge, project)).refresh, "refresh_only");
    await me.click("Refresh", "button");
    const read = await written(bridge, project);
    assert.equal(tabOf(read, "features").text.trim(), FEATURE);
    await me.waitFor("the written tab", (seen) => seen.includes("Sells fruit"));
    await me.capture("refresh-only-written");
  }, 1, CODEX);
});

test("Overview tabs, Off: no run starts, Refresh is disabled and the engine refuses a refresh", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-off-", async ({ project, me, page, site, bridge }) => {
    assert.equal(await refusal(bridge, "desktop_overview_set_refresh", { projectId: project, refresh: "off" }), null);
    await openTab(page, site, me, project, "Features");
    await me.waitFor("the off line", (seen) => seen.includes("Overview tabs are off in Project settings."));
    assert.deepEqual(await button(page, "Refresh"), { disabled: true }, "Refresh is disabled while the tabs are off");
    assert.equal(await refusal(bridge, "desktop_overview_refresh", { projectId: project }), "overview_off");
    await staysUnwritten(bridge, project, 20000);
    await me.click("README", "[role=tab]");
    await me.waitFor("the README, which needs no run", (seen) => seen.includes("Sells fruit.") && seen.includes("This is the project's own file."));
    await me.capture("off");
  }, 1, CODEX);
});

test("Refreshed tabs, Apply when I have not edited the tab: an unedited tab changes and an edited one gets a proposal", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-apply-", async ({ project, me, page, site, bridge }) => {
    assert.equal(await refusal(bridge, "desktop_overview_set_apply", { projectId: project, apply: "when_unedited" }), null);
    await openTab(page, site, me, project, "Features");
    const first = await written(bridge, project);
    const quick = tabOf(first, "quick_info");
    assert.equal(await refusal(bridge, "desktop_overview_save", { projectId: project, tab: "quick_info", text: `${OWNER_LINE}\n`, version: quick.version }), null);
    appendFileSync(join(await repoOf(bridge, project), "README.md"), `\nNow with ${MARKER}.\n`);
    await me.click("Refresh", "button");
    const after = await until(bridge, project, "the unedited tab never changed", (read) => !read.running && tabOf(read, "features").text.includes(ADDED));
    assert.equal(tabOf(after, "features").proposal, null, "an unedited tab is replaced with no question");
    assert.equal(tabOf(after, "quick_info").text.trim(), OWNER_LINE, "the edited tab keeps the owner's text");
    assert.equal(tabOf(after, "quick_info").edited, true);
    assert.equal(tabOf(after, "quick_info").proposal.trim(), QUICK, "the edited tab gets a proposal instead");
    await me.waitFor("the changed tab with no banner", (seen) => seen.includes("Sells juice") && !seen.includes("A refresh proposes changes to this tab."));
    await me.capture("applied-unedited");
  }, 1, CODEX);
});

test("An owner's edit survives a refresh: the tab keeps the owner's text, the refresh waits as a proposal and Reject drops it", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-edit-", async ({ project, me, page, site, bridge }) => {
    await openTab(page, site, me, project, "Features");
    await written(bridge, project);
    await me.waitFor("the written tab", (seen) => seen.includes("Sells fruit"));
    await me.click("Edit", "button");
    await me.type("#ov-edit", `${OWNER_LINE}\n`);
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings` });
    await me.waitFor("the Overview left behind", (seen) => !seen.includes("Edit Features"));
    await openTab(page, site, me, project, "Features");
    await me.waitFor("the editor reopened", (seen) => seen.includes("Edit Features"));
    assert.ok((await page.evaluate(`document.querySelector("#ov-edit")?.value ?? ""`)).includes(OWNER_LINE), "the typed text survived leaving the Overview");
    await me.click("Save", "button");
    const saved = await until(bridge, project, "Save never stored the owner's text", (read) => tabOf(read, "features").edited && tabOf(read, "features").text.includes(OWNER_LINE), STEP_MS);
    const mine = tabOf(saved, "features").text;
    await me.click("Refresh", "button");
    const proposed = await until(bridge, project, "the refresh never proposed for the edited tab", (read) => !read.running && tabOf(read, "features").proposal !== null);
    assert.equal(tabOf(proposed, "features").text, mine, "the refresh did not touch the owner's text");
    await me.waitFor("the banner", (seen) => seen.includes("A refresh proposes changes to this tab."));
    await me.click("Review", "button");
    await me.waitFor("the edited-tab warning", (seen) => seen.includes("You edited this tab. Accept replaces your text with the proposal; Reject keeps yours."));
    await me.capture("edited-proposal");
    await me.click("Reject", "button");
    const kept = await until(bridge, project, "Reject never dropped the proposal", (read) => tabOf(read, "features").proposal === null, STEP_MS);
    assert.equal(tabOf(kept, "features").text, mine, "Reject keeps the owner's text");
  }, 1, CODEX);
});

test("Run research: the Competitors tab starts competitor research only when asked and fills after the next refresh", { timeout: 1200000 }, async (t) => {
  await onEngine(t, "ca-overview-research-", async ({ project, me, page, site, bridge }) => {
    const before = await value(bridge, "desktop_project_setup", { id: project });
    if (before.research.includes("competitor_research")) {
      const setup = { about: before.about, tech_stack: before.tech_stack, platforms: before.platforms, research: before.research.filter((kind) => kind !== "competitor_research"), pool: before.pool, team: before.team, documents: [] };
      assert.equal((await value(bridge, "desktop_update_project_setup", { id: project, setup, name: null, revision: before.revision, generation: 900 })).outcome, "applied");
    }
    await openTab(page, site, me, project, "Competitors");
    await me.waitFor("the explanation", (seen) => seen.includes("There is no competitor research for this project yet."));
    assert.deepEqual(await button(page, "Run research"), { disabled: false });
    await pause(10000);
    assert.ok(!(await value(bridge, "desktop_project_setup", { id: project })).research.includes("competitor_research"), "nothing starts on its own");
    await me.click("Run research", "button");
    await me.waitFor("the research under way or done", (seen) => seen.includes("Competitor research is running.") || seen.includes("Written from the competitor research report."), RUN_MS);
    assert.ok((await value(bridge, "desktop_project_setup", { id: project })).research.includes("competitor_research"), "Run research picked competitor research in the project setup");
    await until(bridge, project, "competitor research never produced a report", (read) => read.competitors.kind === "report");
    assert.equal(await button(page, "Run research"), null, "Run research leaves once there is research");
    await me.click("Refresh", "button");
    const filled = await until(bridge, project, "the Competitors tab never filled", (read) => !read.running && (tabOf(read, "competitors").text ?? tabOf(read, "competitors").proposal ?? "").includes("Berry Barn"));
    assert.equal(filled.competitors.kind, "report");
    await me.capture("competitors");
  }, 1, CODEX);
});

test("A tab file CrazyAgent can't show says why, names the file and has no Edit", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-blocked-", async ({ project, me, page, site, bridge }) => {
    await openTab(page, site, me, project, "Features");
    await written(bridge, project);
    writeFileSync(join(await repoOf(bridge, project), ".crazyagent", "overview", "features.md"), Buffer.from([0xff, 0xfe, 0x0a]));
    const blocked = await until(bridge, project, "the engine never reported the unreadable file", (read) => tabOf(read, "features").blocked === "not_utf8", STEP_MS);
    assert.equal(tabOf(blocked, "features").text, null);
    await openTab(page, site, me, project, "Features");
    const seen = await me.waitFor("the blocked line", (text) => text.includes("CrazyAgent can't show this tab: its file is not plain text."));
    assert.ok(seen.includes(".crazyagent/overview/features.md"), "the line names the file");
    assert.equal(await button(page, "Edit"), null, "a blocked tab has no Edit");
    assert.equal(await refusal(bridge, "desktop_overview_save", { projectId: project, tab: "features", text: "- x\n", version: "" }), "overview_tab_blocked");
    await me.capture("blocked");
  }, 1, CODEX);
});

test("A tab file changed outside CrazyAgent shows its new text when its tab is picked again, with no new visit", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-overview-outside-", async ({ project, me, page, site, bridge }) => {
    await openTab(page, site, me, project, "Features");
    await written(bridge, project);
    await me.waitFor("the written Features tab from a read after the run, so the page polls no more", (seen) => seen.includes("Sells fruit") && /Checked /.test(seen));
    writeFileSync(join(await repoOf(bridge, project), ".crazyagent", "overview", "features.md"), `- ${OUTSIDE}\n`);
    await until(bridge, project, "the engine never read the outside edit", (read) => (tabOf(read, "features").text ?? "").includes(OUTSIDE), STEP_MS);
    await me.click("Quick info", "[role=tab]");
    await me.click("Features", "[role=tab]");
    await me.waitFor("the outside edit on the Features tab", (text) => text.includes(OUTSIDE));
    await me.capture("outside-edit");
  }, 1, CODEX);
});
