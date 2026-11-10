import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import { join } from "node:path";
import { click, says, shown, text, texts, type, unfold } from "./settings-steps.mjs";
import { MODE, NAME, PROJECT, TEAM_MS, UI_MS, choose, go, graph, hash, inView, lit, paste, pick, project, reopen, run, screen, segment, tab, team, until, waitFor } from "./trial-kit.mjs";
import { sendToMaster, terminalLive } from "./trial4/n-d8-master-reply.mjs";

const ANSWER = process.env.TRIAL_ANSWER ?? "Hello";
const POOL = process.env.TRIAL_POOL ?? "";
const SETUP_MS = Number(process.env.TRIAL_SETUP_MS ?? 1800000);
const MASTER_MS = Math.min(TEAM_MS, 600000);
const STOP_MS = Math.min(TEAM_MS, 300000);
const FIXTURE_INSTRUCTIONS = "# Mandatory workflow\n\nFixture instruction set for the trial 3 offline proof.\n";
const INSTRUCTIONS = { first: "# Mandatory workflow", bytes: MODE === "fixture" ? Buffer.byteLength(FIXTURE_INSTRUCTIONS) : 12848 };
export const SETTINGS = { landOn: process.env.TRIAL_LAND_ON ?? "trial3-land", background: "wait", worker: "Chosen skills", nudges: "First edit", finished: "Show", network: "https" };
const RUNNING = ["starting", "idle", "working"];
/** The kit calls that the trial 4 checks' shared master helpers make. */
const MASTER_KIT = { paste, until, waitFor };
const WORKER_BOX = "//*[@role=\"group\"][@aria-label=\"Skills per role\"]/div[.//*[@aria-label=\"Worker skills\"]]//label[.//input[@type=\"checkbox\"]]";
const STATE_WORDS = ["Ready to review", "Conflicts with your branch", "Agent is fixing conflicts with main", "Agent is updating", "Accepted", "Discarded", "Landed outside the app"];
assert(Number.isFinite(SETUP_MS) && SETUP_MS > 0, "the setup deadline is positive");
assert.match(SETTINGS.landOn, /^trial[0-9]{1,2}-land$/, "TRIAL_LAND_ON names the trial<n>-land branch the runner seeded");

/** Reads the worker skill boxes the Agents tab shows. Output: [{ name, on }] in screen order. */
async function workerSkills() {
  const boxes = [];
  for (const label of await $$(WORKER_BOX)) boxes.push({ name: (await label.getText()).trim(), on: await label.$("input").isSelected() });
  return boxes;
}

/** Ticks or unticks one worker skill and waits until the box reads that way with its save
 * done. Inputs: the skill name and whether it should be ticked. */
async function tick(name, on) {
  const label = `${WORKER_BOX}[normalize-space(.)=${lit(name)}]`;
  const box = `${label}//input`;
  if (await $(box).isSelected() === on) return;
  await click(label);
  await until(async () => await $(box).isSelected() === on && await $(box).isEnabled(), UI_MS, `${name} did not read ${on ? "ticked" : "unticked"}`);
}

/** Whether a node has ended: stopped or failed, or a master or open teammate with no
 * agent row. Input: the node. Output: a boolean. */
function ended(node) {
  if (node.kind === "teammate" && node.agent == null) return node.status === "stopped";
  if (node.kind === "master" && node.status == null) return true;
  return !RUNNING.includes(node.status);
}

/** Describes the team the trial expects: the master, one lead whose children are two or
 * more native teammates and another whose children are two or more app-spawned sub-agents,
 * each shown as a card. Input: the graph read. Output: { done, seen, leads }. */
function teamShape({ nodes, cards }) {
  const children = (lead) => nodes.filter((node) => node.parent === lead.id);
  const leads = nodes.filter((node) => node.kind === "lead").map((lead) => {
    const kids = children(lead);
    return { lead, mates: kids.filter((node) => node.kind === "teammate").length, subs: kids.filter((node) => node.kind === "sub").length };
  });
  const native = leads.find((row) => row.mates >= 2);
  const spawned = leads.find((row) => row !== native && row.subs >= 2);
  const seen = `${nodes.some((node) => node.kind === "master") ? "master" : "no master"}; leads: ${leads.map((row) => `${row.lead.label} (${row.lead.harness}) ${row.mates} teammates, ${row.subs} sub-agents`).join("; ") || "none"}; cards: ${JSON.stringify(cards)}`;
  const done = nodes.some((node) => node.kind === "master") && Boolean(native && spawned)
    && cards.master >= 1 && cards.lead >= 2 && cards.teammate >= 2 && cards.sub >= 2;
  return { done, seen, leads: native && spawned ? [native.lead.label, spawned.lead.label] : [] };
}

/** Finishes the setup page: waits for the mapping agent's draft or its failure, fills a
 * missing name and confirms. Output: the setup line the page showed. */
async function finishSetup() {
  const confirm = "//button[normalize-space(.)=\"Confirm and finish setup\"]";
  const ready = async () => await shown(confirm) && await $(confirm).isEnabled();
  await until(async () => await ready() || await shown("button=Skip mapping"), SETUP_MS, "setup never offered Confirm and finish setup");
  let skipped = false;
  if (!(await ready())) {
    await click("button=Skip mapping");
    skipped = true;
    await until(ready, UI_MS, "Confirm and finish setup stayed off after Skip mapping");
  }
  const line = await text("p.pj-secondary[role=status]");
  if (await shown("#ob-name") && (await $("#ob-name").getValue()).trim() === "") await type("#ob-name", NAME);
  await click(confirm);
  return `${skipped ? "skipped the failed mapping; " : ""}${line}`;
}

/** Reads the project id from a project route. Output: the id once the window shows the
 * project's Overview. */
async function openedProject() {
  let route = "";
  await until(async () => /^#\/projects\/[^/]+\/overview$/.test(route = await hash()), UI_MS * 2, "the project's Overview did not open");
  return decodeURIComponent(route.split("/")[2]);
}

/** The trial prompt from scripts/trial/prompt.md with {{PROJECT}} replaced by the project
 * folder. Output: the text; throws when a placeholder is left. */
function prompt() {
  const rendered = readFileSync(process.env.TRIAL_PROMPT, "utf8").replaceAll("{{PROJECT}}", PROJECT).trimEnd();
  assert(rendered.length > 0, "the trial prompt is not empty");
  assert(!/\{\{[A-Z_]+\}\}/.test(rendered), "the trial prompt has no placeholder left");
  return rendered;
}

/** The owner actions of trial 3 in run order, each { slug, it, title, why, action }: the
 * slug names its screenshot and is what a trial 4 check's "after" names, it is the mocha test
 * name, and action returns what the step saw or throws. trial3.e2e.mjs and trial4.e2e.mjs run
 * them through the kit's step(). */
export const TRIAL3_STEPS = [
  {
    slug: "open-app",
    it: "opens the app on Home",
    title: "Open the app on Home",
    why: "DEC-112: every owner action of trial 3 runs in the real desktop app, starting from Home",
    action: async () => {
      await until(async () => await shown("#profile-name") || await shown("section.in-found"), 120000, "the app showed neither first run nor Home");
      if (await shown("#profile-name")) throw new Error("first run is showing: the app is not on an onboarded owner profile");
      return "Home is shown with the projects folder";
    },
  },
  {
    slug: "add-project",
    it: "adds the scratch project",
    title: `Add the scratch project ${NAME}`,
    why: "Trial 3 runs on a fresh scratch project the runner seeded in the projects folder; the owner sets it up from Home and leaves the instruction import's CLAUDE.md move uncommitted, so main stays at the seed commit the audit's Land on check reads",
    action: async () => {
      const row = `//li[contains(concat(" ", normalize-space(@class), " "), " in-row ")][.//strong[normalize-space(.)=${lit(NAME)}]]`;
      await until(() => shown(row), 60000, `Home never listed ${NAME} in the projects folder`);
      const action = (await $(`${row}//button`).getText()).trim();
      await click(`${row}//button`);
      let line = "";
      if (action === "Set up this project") {
        await until(() => says("dialog[open] h2", `Set up ${NAME}`), UI_MS, "the Set up dialog did not open");
        if (POOL) await click(`//dialog[@open]//label[.//*[normalize-space(.)=${lit(POOL)}]]`);
        await click("//dialog[@open]//button[normalize-space(.)=\"Start setup\"]", 60000);
        line = await finishSetup();
      } else if (action === "Continue setup") {
        line = await finishSetup();
      } else {
        assert.equal(action, "Open project", `the row offers Set up, Continue or Open: ${action}`);
      }
      run.project = await openedProject();
      const move = await shown("button=Commit the move")
        ? "; the instruction import moved the project's CLAUDE.md and its move stays uncommitted, so main keeps the seed commit" : "";
      return `${action}${line ? `; setup said: ${line}` : ""}; the project's Overview is open${move}`;
    },
  },
  {
    slug: "process-instructions",
    it: "shows the new instruction set on the Process page",
    title: "Process page shows the instruction set",
    why: "Part 1 instruction set (first line # Mandatory workflow, 12848 bytes) is what every agent gets (owner R2)",
    action: async () => {
      await go("#/settings/process");
      await until(() => shown("#process-app"), UI_MS, "the app instructions editor was never shown");
      if (MODE === "fixture") {
        await type("#process-app", FIXTURE_INSTRUCTIONS);
        await click("//form[.//*[@id=\"process-app\"]]//button[normalize-space(.)=\"Save\"]");
        await until(async () => !(await text("form")).includes("Not saved yet."), UI_MS, "the fixture instruction set was not saved");
        await go("#/settings/profile");
        await go("#/settings/process");
        await until(() => shown("#process-app"), UI_MS, "the app instructions editor was never shown again");
      }
      const value = await $("#process-app").getValue();
      const first = value.split("\n")[0];
      const bytes = Buffer.byteLength(value);
      assert.equal(first, INSTRUCTIONS.first, `the first line is ${INSTRUCTIONS.first}`);
      assert.equal(bytes, INSTRUCTIONS.bytes, `the instruction set is ${INSTRUCTIONS.bytes} bytes`);
      return `${MODE === "fixture" ? "fixture instruction set saved, then " : ""}first line ${first}, ${bytes} bytes`;
    },
  },
  {
    slug: "land-on",
    it: "sets Land on",
    title: `Set Land on to ${SETTINGS.landOn}`,
    why: `18.13 Land on (Q1): the master's merges and Accept land on ${SETTINGS.landOn}, not on main`,
    action: async () => {
      await tab("agents?section=settings");
      const field = `[id="tg-land-on-${project()}"]`;
      const save = "//form[@aria-label=\"Land on\"]//button[normalize-space(.)=\"Save\"]";
      await type(field, SETTINGS.landOn);
      await click(save);
      await until(async () => !(await $(save).isEnabled()), UI_MS, "Land on Save did not finish");
      await reopen("agents?section=settings");
      await until(async () => await shown(field) && (await $(field).getValue()) === SETTINGS.landOn, UI_MS, "Land on did not read back");
      await inView(field);
      return `Land on reads ${SETTINGS.landOn} after reopening the Agents tab`;
    },
  },
  {
    slug: "background",
    it: "sets background commands",
    title: "Set Background commands at task end to Wait",
    why: "18.13 background commands (Q13, S3): under Wait a task stays open until its background command ends",
    action: async () => {
      await tab("agents?section=settings");
      const field = `[id="tg-background-${project()}"]`;
      const changed = await pick(field, SETTINGS.background);
      await reopen("agents?section=settings");
      await until(async () => await shown(field) && (await $(field).getValue()) === SETTINGS.background, UI_MS, "Background commands did not read back");
      await inView(field);
      return `Background commands reads Wait for them after reopening${changed ? "" : " (it already was)"}`;
    },
  },
  {
    slug: "skills-per-role",
    it: "sets skills per role",
    title: "Set Worker skills to Chosen skills with one skill",
    why: "18.13 skills per role (R3): workers get only the one approved skill the owner ticks, a real subset the audit checks",
    action: async () => {
      const done = await choose("agents?section=settings", "Worker skills", SETTINGS.worker);
      const none = "//*[@role=\"group\"][@aria-label=\"Skills per role\"]/div[.//*[@aria-label=\"Worker skills\"]]//*[normalize-space(.)=\"No approved skills for this project yet.\"]";
      await until(async () => (await workerSkills()).length > 0 || await shown(none), UI_MS, "the worker skill boxes were never shown");
      const boxes = await workerSkills();
      if (boxes.length === 0) throw new Error("no approved skill to choose: Chosen skills for workers needs at least one");
      const keep = boxes[0].name;
      for (const box of boxes) await tick(box.name, box.name === keep);
      await reopen("agents?section=settings");
      await until(async () => (await workerSkills()).length === boxes.length, UI_MS, "the worker skill boxes did not come back after reopening");
      const after = await workerSkills();
      assert.deepEqual(after.filter((box) => box.on).map((box) => box.name), [keep], `workers read only ${keep} after reopening`);
      await inView(WORKER_BOX);
      const master = await $(segment("Master skills", "All approved skills")).getAttribute("aria-checked");
      const lead = await $(segment("Lead skills", "All approved skills")).getAttribute("aria-checked");
      const only = after.length === 1 ? " (the only approved skill, so this cannot show a subset)" : "";
      return `${done}; workers get only ${keep}: 1 of ${after.length} approved skills ticked after reopening${only}; master all: ${master}, lead all: ${lead}`;
    },
  },
  {
    slug: "nudges",
    it: "sets workflow nudges",
    title: "Set Workflow nudges to First edit",
    why: "18.13 nudges (R4): an agent editing before any graph query gets one nudge",
    action: () => choose("agents?section=settings", "Workflow nudges", SETTINGS.nudges),
  },
  {
    slug: "finished-agents",
    it: "shows finished agents",
    title: "Set Finished agents to Show",
    why: "The final tree must show the stopped agents, so finished agents stay in the tree",
    action: () => choose("agents?section=settings", "Finished agents", SETTINGS.finished),
  },
  {
    slug: "network",
    it: "sets the agent network",
    title: "Set agent network to Port 443 only",
    why: "Agent access: agents with tools reach the model APIs on port 443 and nothing else",
    action: async () => {
      const section = async () => {
        await tab("settings");
        await click("//nav[@aria-label=\"Project settings\"]//button[normalize-space(.)=\"Agent access\"]");
      };
      const radio = `input[name="ps-network"][value="${SETTINGS.network}"]`;
      await section();
      await until(() => shown("//*[@role=\"radiogroup\"][@aria-label=\"Agent network\"]"), UI_MS, "the agent network choices were never shown");
      const was = await $(radio).isSelected();
      if (!was) {
        await click(`//label[.//input[@name="ps-network" and @value="${SETTINGS.network}"]]`);
        await click("button=Save agent access");
        await until(async () => !(await $("//button[normalize-space(.)=\"Save agent access\"]").isEnabled()), UI_MS, "Save agent access did not finish");
      }
      await tab("overview");
      await section();
      await until(async () => await shown("//*[@role=\"radiogroup\"][@aria-label=\"Agent network\"]") && await $(radio).isSelected(), UI_MS, "the network did not read back");
      await inView("//*[@role=\"radiogroup\"][@aria-label=\"Agent network\"]");
      return `network reads Port 443 only after reopening Settings, Agent access${was ? " (it already was)" : ""}`;
    },
  },
  {
    slug: "hidden-folders",
    it: "hides the private folder",
    title: "Hide the private folder from agents",
    why: "18.13 hidden folders (Q7, S5): agents cannot read private/ while access is Sandboxed",
    action: async () => {
      const folder = join(realpathSync(PROJECT), "private");
      const section = async () => {
        await tab("settings");
        await click("//nav[@aria-label=\"Project settings\"]//button[normalize-space(.)=\"Sandbox\"]");
      };
      const heading = "//h2[normalize-space(.)=\"Folders agents can't open\"]";
      const list = "ul[aria-label=\"Hidden folders\"]";
      await section();
      try {
        await until(() => shown(heading), UI_MS, "no section");
      } catch {
        throw new Error("not in this build: Settings, Sandbox has no Folders agents can't open section");
      }
      await until(() => shown(list), UI_MS, "the hidden folders list was never shown");
      const was = (await text(list)).includes(folder);
      if (!was) {
        await type("#ps-hidden-path", folder);
        await click("button=Hide folder");
        await click("button=Save hidden folders");
        await until(async () => !(await $("//button[normalize-space(.)=\"Save hidden folders\"]").isEnabled()), UI_MS, "Save hidden folders did not finish");
      }
      await tab("overview");
      await section();
      await until(async () => await shown(list) && (await text(list)).includes(folder), UI_MS, "the hidden folder did not read back");
      await inView(list);
      return `Hidden folders lists ${folder} after reopening Settings, Sandbox${was ? " (it already did)" : ""}`;
    },
  },
  {
    slug: "send-prompt",
    it: "sends the master the trial prompt",
    title: "Send the master the trial prompt",
    why: "The trial prompt (scripts/trial/prompt.md) asks for two leads, one native Claude and one app-spawned, two sub-agents each, one question for the owner and merges",
    action: async () => {
      const message = prompt();
      await tab("conversation");
      const started = await shown("button=Start all");
      if (started) await click("button=Start all");
      await terminalLive(MASTER_KIT, MASTER_MS);
      const way = await sendToMaster(MASTER_KIT, message);
      return `${started ? "Start all, then " : ""}sent ${Buffer.byteLength(message)} bytes by ${way} (sha256 ${createHash("sha256").update(message).digest("hex").slice(0, 12)})`;
    },
  },
  {
    slug: "team-tree",
    it: "shows the team tree",
    title: "Team tree shows the master, two leads and two sub-agents each",
    why: "The prompt's team: a native Claude lead with teammates and an app-spawned lead with sub-agents (DEC-104)",
    action: async () => {
      const wait = team();
      await tab("agents");
      let shape = { leads: [] };
      const seen = await waitFor(async () => {
        shape = teamShape(await graph());
        return shape;
      }, wait, "the team tree never showed the trial team");
      run.leads = shape.leads;
      return seen;
    },
  },
  {
    slug: "question",
    it: "answers the agent's question",
    title: "Questions panel shows the agent's question; answer it",
    why: "A For: Me question reaches only the owner (trial 2 check 4); the answer goes back to the asker",
    action: async () => {
      const wait = team();
      await tab("agents");
      const items = "ul[aria-label=\"Open questions\"] li.tg-question";
      await waitFor(async () => {
        const count = (await texts(items)).length;
        return { done: count > 0, seen: `${count} open questions` };
      }, wait, "no agent question reached the Questions panel");
      const before = (await texts(items)).length;
      const asked = (await texts(`${items} p.pj-pre`))[0] ?? "";
      await click("(//ul[@aria-label=\"Open questions\"]//li[contains(@class, \"tg-question\")])[1]//button[normalize-space(.)=\"Answer\"]");
      await type("//textarea[@id = //label[starts-with(normalize-space(.), \"Your answer to\")]/@for]", ANSWER);
      await click("button=Send answer");
      await until(async () => (await texts(items)).length < before, 60000, "the answered question stayed open");
      return `question: ${asked.slice(0, 300)}; answered: ${ANSWER}; ${before - 1} still open`;
    },
  },
  {
    slug: "changes",
    it: "shows the merges in Changes",
    title: "Changes shows the merges",
    why: `Each lead's merged work shows as a change set that lands on ${SETTINGS.landOn} (Land on, DEC-095)`,
    action: async () => {
      const wait = team();
      await tab("changes");
      const rows = "ul[aria-label=\"Change sets\"] > li";
      return waitFor(async () => {
        const all = await texts(rows);
        const words = STATE_WORDS.concat("no state").map((word) => [word, all.filter((row) => (STATE_WORDS.find((state) => row.includes(state)) ?? "no state") === word).length])
          .filter(([, count]) => count > 0).map(([word, count]) => `${word} x${count}`).join(", ");
        return { done: all.length >= 2 && !all.some((row) => row.includes("Agent is updating") || row.includes("Agent is fixing conflicts with main")), seen: `${all.length} change sets: ${words || "none"}; leads ${run.leads.join(", ") || "unknown"}` };
      }, wait, "Changes never showed a settled change set for each lead");
    },
  },
  {
    slug: "stop-all",
    it: "stops every agent",
    title: "Stop all",
    why: "The owner ends the trial with Stop all; every agent stops (trial 2 check 10)",
    action: async () => {
      await tab("agents");
      await unfold("Agents and tasks");
      await click("button=Stop all");
      await until(() => says("dialog[open] h2", "Stop every agent of this project?"), UI_MS, "the Stop all question did not open");
      await click("//dialog[@open]//button[normalize-space(.)=\"Stop all\"]");
      await until(async () => !(await shown("dialog[open]")), 60000, "the Stop all dialog stayed open");
      return "Stop all confirmed";
    },
  },
  {
    slug: "final-tree",
    it: "shows every agent stopped",
    title: "Final tree shows every agent stopped",
    why: "After Stop all no agent of the project runs",
    action: async () => {
      await tab("agents");
      return waitFor(async () => {
        const { nodes } = await graph();
        const running = nodes.filter((node) => !ended(node));
        return { done: nodes.length > 1 && running.length === 0, seen: `${nodes.length} nodes; still running: ${running.map((node) => `${node.label} ${node.status ?? "open"}`).join(", ") || "none"}` };
      }, STOP_MS, "agents were still running after Stop all");
    },
  },
];
