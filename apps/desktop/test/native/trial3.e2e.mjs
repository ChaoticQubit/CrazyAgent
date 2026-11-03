import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, readFileSync, realpathSync } from "node:fs";
import { basename, join } from "node:path";
import { SECRET_SOURCES, mask } from "./trial3-mask.mjs";
import { click, says, shot, shown, text, texts, type, until } from "./settings-steps.mjs";

const MODE = process.env.TRIAL3_MODE;
const EVIDENCE = process.env.TRIAL_EVIDENCE;
const PROJECT = process.env.TRIAL_PROJECT;
const NAME = basename(PROJECT);
const STEPS = join(EVIDENCE, "steps.jsonl");
const ANSWER = process.env.TRIAL_ANSWER ?? "Hello";
const POOL = process.env.TRIAL_POOL ?? "";
const TEAM_MS = Number(process.env.TRIAL_TEAM_MS ?? 7200000);
const SETUP_MS = Number(process.env.TRIAL_SETUP_MS ?? 1800000);
const MASTER_MS = Math.min(TEAM_MS, 600000);
const STOP_MS = 300000;
const UI_MS = 30000;
const FIXTURE_INSTRUCTIONS = "# Mandatory workflow\n\nFixture instruction set for the trial 3 offline proof.\n";
const INSTRUCTIONS = { first: "# Mandatory workflow", bytes: MODE === "fixture" ? Buffer.byteLength(FIXTURE_INSTRUCTIONS) : 12848 };
const SETTINGS = { landOn: "trial3-land", background: "wait", claude: "native", worker: "Chosen skills", nudges: "First edit", finished: "Show", network: "https", input: "Both" };
const RUNNING = ["starting", "idle", "working"];
const STATE_WORDS = ["Ready to review", "Conflicts with your branch", "Agent is updating", "Accepted", "Discarded", "Landed outside the app"];
assert(["live", "fixture"].includes(MODE), "TRIAL3_MODE is live or fixture");
assert(Number.isFinite(TEAM_MS) && TEAM_MS > 0 && Number.isFinite(SETUP_MS) && SETUP_MS > 0, "the trial deadlines are positive");
const run = { desktop: 0, project: null, leads: [], input: null };

/** Quotes text as an XPath string literal. Input: text without a double quote. Output: it quoted. */
function lit(value) {
  assert(!value.includes('"'), "XPath text has no double quote");
  return `"${value}"`;
}

/** The XPath of one option button of a segmented control. Inputs: the control's label and
 * the option's text. Output: the selector. */
function segment(group, option) {
  return `//*[@role="radiogroup"][@aria-label=${lit(group)}]//button[normalize-space(.)=${lit(option)}]`;
}

/** Appends one masked desktop line to steps.jsonl, numbered after every line already there,
 * so the TUI recorder's lines and these share one order. Input: the line without its n. */
function record(line) {
  const before = existsSync(STEPS) ? readFileSync(STEPS, "utf8").split("\n").filter(Boolean).length : 0;
  appendFileSync(STEPS, `${JSON.stringify({ n: before + 1, surface: "desktop", ...line, detail: mask(line.detail) })}\n`, { mode: 0o600 });
}

/** Masks every secret shape in the window's text and form values and hides terminal
 * canvases, whose text cannot be masked, so the next screenshot holds no secret. */
function maskWindow() {
  return browser.execute((sources) => {
    const patterns = sources.map((source) => new RegExp(source, "g"));
    const clean = (value) => patterns.reduce((out, pattern) => out.replace(pattern, "[masked]"), value);
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const next = clean(node.nodeValue);
      if (next !== node.nodeValue) node.nodeValue = next;
    }
    for (const field of document.querySelectorAll("input, textarea")) {
      const next = clean(field.value);
      if (next !== field.value) field.value = next;
    }
    for (const canvas of document.querySelectorAll(".tg-terminal-screen canvas")) canvas.style.visibility = "hidden";
  }, SECRET_SOURCES);
}

/** Shows the terminal canvases maskWindow hid once the screenshot is taken. */
function unhideTerminals() {
  return browser.execute(() => {
    for (const canvas of document.querySelectorAll(".tg-terminal-screen canvas")) canvas.style.visibility = "";
  });
}

/** Runs one owner step: the action, then a masked screenshot NN-<slug>.png on success or
 * failure, then its steps.jsonl line. Inputs: the slug, the title, why the trial does it and
 * the action, which returns what it saw. Throws when the action or the screenshot failed. */
async function step(slug, title, why, action) {
  run.desktop += 1;
  const name = `${String(run.desktop).padStart(2, "0")}-${slug}`;
  assert.match(name, /^[a-z0-9-]{1,40}$/);
  const at = new Date().toISOString();
  let ok = true;
  let detail;
  try {
    detail = String(await action());
  } catch (failure) {
    ok = false;
    detail = String(failure?.message ?? failure);
  }
  const evidence = [];
  try {
    await maskWindow();
    await shot(name);
    await unhideTerminals();
    evidence.push(`screenshots/${name}.png`);
  } catch (failure) {
    ok = false;
    detail = `${detail}; no screenshot: ${failure?.message ?? failure}`;
  }
  record({ title, why, at, ok, detail, evidence });
  if (!ok) throw new Error(`${title}: ${mask(detail)}`);
}

/** Waits for a condition with a deadline and fails with the last thing it observed.
 * Inputs: the condition, which returns { done, seen }, the deadline in ms and the label. */
async function waitFor(condition, timeout, label) {
  let seen = "nothing read yet";
  try {
    await browser.waitUntil(async () => {
      const result = await condition();
      seen = result.seen;
      return result.done;
    }, { timeout, interval: 1000 });
  } catch {
    throw new Error(`${label} within ${Math.round(timeout / 1000)} s; last seen: ${seen}`);
  }
  return seen;
}

/** The window's route. Output: location.hash. */
function hash() {
  return browser.execute(() => location.hash);
}

/** Opens a route by its hash and waits until the window shows it. Input: the hash. */
async function go(route) {
  await browser.execute((next) => { location.hash = next; }, route);
  await until(async () => (await hash()) === route, UI_MS, `${route} did not open`);
}

/** The trial project's id, read by step 2. Output: the id; throws when no step found it. */
function project() {
  assert(run.project, "needs the project id from the add-project step");
  return run.project;
}

/** Opens one tab of the trial project. Input: the tab id. */
function tab(id) {
  return go(`#/projects/${encodeURIComponent(project())}/${id}`);
}

/** Opens another tab and comes back, so the tab reads what the engine saved rather than
 * what the form still holds. Input: the tab id. */
async function reopen(id) {
  await tab("overview");
  await tab(id);
}

/** Sets a native select as the owner's choice does, firing its change event. Inputs: the
 * selector and the option value. Output: whether the value changed. */
async function pick(selector, value) {
  await until(() => shown(selector), UI_MS, `${selector} was never shown`);
  return browser.execute((selector, value) => {
    const field = document.querySelector(selector);
    if (![...field.options].some((option) => option.value === value)) throw new Error(`${selector} has no option ${value}`);
    if (field.value === value) return false;
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(field, value);
    field.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }, selector, value);
}

/** Puts text into a React text area as one paste, so a newline never presses Enter and
 * sends early. Inputs: the selector and the text. */
async function paste(selector, value) {
  await browser.execute((selector, value) => {
    const box = [...document.querySelectorAll(selector)].find((node) => node.getClientRects().length);
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(box, value);
    box.dispatchEvent(new Event("input", { bubbles: true }));
  }, selector, value);
  await until(async () => (await $(selector).getValue()) === value, UI_MS, `${selector} did not keep the pasted text`);
}

/** Picks one option of a segmented control and checks it reads as chosen after the tab is
 * opened again. Inputs: the tab, the control's label and the option. Output: what was done. */
async function choose(where, group, option) {
  await until(() => shown(segment(group, option)), UI_MS, `${group} was never shown`);
  const was = await $(segment(group, option)).getAttribute("aria-checked") === "true";
  if (!was) await click(segment(group, option));
  await until(async () => await $(segment(group, option)).getAttribute("aria-checked") === "true", UI_MS, `${group} did not take ${option}`);
  await reopen(where);
  await until(async () => await shown(segment(group, option)) && await $(segment(group, option)).getAttribute("aria-checked") === "true", UI_MS, `${group} did not read ${option} after reopening`);
  return `${group} reads ${option} after reopening the tab${was ? " (it already was)" : ""}`;
}

/** Reads the project's team graph from the engine through the app's own command and the
 * cards the Agents tab draws. Output: the nodes and the card counts by kind. */
async function graph() {
  const answer = await browser.executeAsync((id, done) => {
    window.__TAURI_INTERNALS__.invoke("desktop_team_graph", { id }).then(done, (failure) => done({ failed: String(failure) }));
  }, project());
  const nodes = answer?.nodes ?? answer?.value?.nodes;
  assert(Array.isArray(nodes), `desktop_team_graph answered no nodes: ${JSON.stringify(answer).slice(0, 300)}`);
  const cards = await browser.execute(() => Object.fromEntries(["master", "lead", "sub", "teammate"]
    .map((kind) => [kind, [...document.querySelectorAll(`.tg-card.${kind}`)].filter((node) => node.getClientRects().length).length])));
  return { nodes, cards };
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

/** Goes through first run on the fixture's empty profile: saves a profile, keeps the
 * suggested projects folder and skips tools and accounts. */
async function firstRun() {
  await type("#profile-name", "Trial Owner");
  await type("#profile-role", "Founder");
  await type("#profile-bio", "Runs the trial 3 offline proof.");
  await click("button=Save and continue");
  await until(() => says("h1", "Choose your projects folder"), UI_MS, "the projects folder step did not open");
  await click("button=Continue");
  await until(() => says("h1", "Set up tools"), UI_MS, "the tools step did not open");
  await until(async () => await shown("button=Skip for now") || await shown("button=Continue"), UI_MS, "the tools step offered no way on");
  await click(await shown("button=Skip for now") ? "button=Skip for now" : "button=Continue");
  await until(() => says("h1", "Add an account"), UI_MS, "the accounts step did not open");
  await click("button=Skip for now");
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

describe(`CrazyAgent trial 3 owner actions on the desktop app (${MODE})`, () => {
  it("opens the app on Home", () => step("open-app", "Open the app on Home",
    "DEC-112: every owner action of trial 3 runs in the real desktop app, starting from Home", async () => {
      await until(async () => await shown("#profile-name") || await shown("section.in-found"), 120000, "the app showed neither first run nor Home");
      if (!(await shown("#profile-name"))) return "Home is shown with the projects folder";
      if (MODE === "live") throw new Error("first run is showing: the app is not on the owner's profile");
      await firstRun();
      await until(() => shown("section.in-found"), UI_MS, "Home did not open after first run");
      return "fixture only: first run saved a profile, kept the projects folder and skipped tools and accounts; Home is shown";
    }));

  it("adds the scratch project", () => step("add-project", `Add the scratch project ${NAME}`,
    "Trial 3 runs on a fresh scratch project the runner seeded in the projects folder; the owner sets it up from Home", async () => {
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
      return `${action}${line ? `; setup said: ${line}` : ""}; the project's Overview is open`;
    }));

  it("shows the new instruction set on the Process page", () => step("process-instructions", "Process page shows the instruction set",
    "Part 1 instruction set (first line # Mandatory workflow, 12848 bytes) is what every agent gets (owner R2)", async () => {
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
    }));

  it("sets Land on", () => step("land-on", `Set Land on to ${SETTINGS.landOn}`,
    "18.13 Land on (Q1): the master's merges and Accept land on trial3-land, not on main", async () => {
      await tab("agents");
      const field = `[id="tg-land-on-${project()}"]`;
      const save = "//form[@aria-label=\"Land on\"]//button[normalize-space(.)=\"Save\"]";
      await type(field, SETTINGS.landOn);
      await click(save);
      await until(async () => !(await $(save).isEnabled()), UI_MS, "Land on Save did not finish");
      await reopen("agents");
      await until(async () => await shown(field) && (await $(field).getValue()) === SETTINGS.landOn, UI_MS, "Land on did not read back");
      return `Land on reads ${SETTINGS.landOn} after reopening the Agents tab`;
    }));

  it("sets background commands", () => step("background", "Set Background commands at task end to Wait",
    "18.13 background commands (Q13, S3): under Wait a task stays open until its background command ends", async () => {
      await tab("agents");
      const field = `[id="tg-background-${project()}"]`;
      const changed = await pick(field, SETTINGS.background);
      await reopen("agents");
      await until(async () => await shown(field) && (await $(field).getValue()) === SETTINGS.background, UI_MS, "Background commands did not read back");
      return `Background commands reads Wait for them after reopening${changed ? "" : " (it already was; no row also means Wait)"}`;
    }));

  it("sets Claude Code sub-agents to native teammates", () => step("claude-native", "Set Claude Code sub-agents to Native teammates",
    "DEC-104: one lead runs native Claude teammates while the other spawns app sub-agents", async () => {
      await tab("agents");
      const field = `[id="tg-mode-claude-code-${project()}"]`;
      const changed = await pick(field, SETTINGS.claude);
      await reopen("agents");
      await until(async () => await shown(field) && (await $(field).getValue()) === SETTINGS.claude, UI_MS, "Claude Code sub-agents did not read back");
      return `Claude Code sub-agents reads Native teammates after reopening${changed ? "" : " (it already was)"}`;
    }));

  it("sets skills per role", () => step("skills-per-role", "Set Worker skills to Chosen skills",
    "18.13 skills per role (R3): each role gets only its chosen approved skills", async () => {
      await tab("agents");
      const done = await choose("agents", "Worker skills", SETTINGS.worker);
      const boxes = "//*[@role=\"group\"][@aria-label=\"Skills per role\"]/div[.//*[@aria-label=\"Worker skills\"]]//input[@type=\"checkbox\"]";
      const states = await Promise.all((await $$(boxes)).map((box) => box.isSelected()));
      const master = await $(segment("Master skills", "All approved skills")).getAttribute("aria-checked");
      const lead = await $(segment("Lead skills", "All approved skills")).getAttribute("aria-checked");
      return `${done}; worker ticks ${states.filter(Boolean).length} of ${states.length} approved skills; master all: ${master}, lead all: ${lead}`;
    }));

  it("sets workflow nudges", () => step("nudges", "Set Workflow nudges to First edit",
    "18.13 nudges (R4): an agent editing before any graph query gets one nudge", () => choose("agents", "Workflow nudges", SETTINGS.nudges)));

  it("shows finished agents", () => step("finished-agents", "Set Finished agents to Show",
    "The final tree must show the stopped agents, so finished agents stay in the tree", () => choose("agents", "Finished agents", SETTINGS.finished)));

  it("sets the agent network", () => step("network", "Set agent network to Port 443 only",
    "Agent access: agents with tools reach the model APIs on port 443 and nothing else", async () => {
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
      return `network reads Port 443 only after reopening Settings, Agent access${was ? " (it already was)" : ""}`;
    }));

  it("hides the private folder", () => step("hidden-folders", "Hide the private folder from agents",
    "18.13 hidden folders (Q7, S5): agents cannot read private/ while access is Sandboxed", async () => {
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
      return `Hidden folders lists ${folder} after reopening Settings, Sandbox${was ? " (it already did)" : ""}`;
    }));

  it("shows the message box under the master terminal", () => step("master-input", "Set Master input to Both",
    "The trial prompt is sent from the message box under the live master terminal", async () => {
      await go("#/settings/profile");
      const options = ["Terminal only", "Message box only", "Both"];
      await until(async () => {
        for (const option of options) if (await shown(segment("Master input", option)) && await $(segment("Master input", option)).getAttribute("aria-checked") === "true") run.input = option;
        return run.input !== null;
      }, UI_MS, "Master input was never shown with a choice");
      if (run.input !== SETTINGS.input) await click(segment("Master input", SETTINGS.input));
      await go("#/settings/process");
      await go("#/settings/profile");
      await until(async () => await shown(segment("Master input", SETTINGS.input)) && await $(segment("Master input", SETTINGS.input)).getAttribute("aria-checked") === "true", UI_MS, "Master input did not read back");
      return `Master input reads Both after reopening Settings, Profile (it was ${run.input})`;
    }));

  it("sends the master the trial prompt", () => step("send-prompt", "Send the master the trial prompt",
    "The trial prompt (scripts/trial/prompt.md) asks for two leads, one native Claude and one app-spawned, two sub-agents each, one question for the owner and merges", async () => {
      const message = prompt();
      await tab("conversation");
      const started = await shown("button=Start all");
      if (started) await click("button=Start all");
      const box = "textarea.tg-composer-input";
      const send = "//form[contains(@class, \"tg-composer\")]//button[normalize-space(.)=\"Send\"]";
      await waitFor(async () => {
        const live = await shown(box) && await $(send).isEnabled();
        return { done: live, seen: live ? "the master terminal is live" : mask((await text("main")).slice(0, 400)) };
      }, MASTER_MS, "the master's message box never became ready to send");
      await paste(box, message);
      await click(send);
      await until(async () => (await $(box).getValue()) === "", UI_MS, "the message box kept the prompt: it was not sent");
      return `${started ? "Start all, then " : ""}sent ${Buffer.byteLength(message)} bytes (sha256 ${createHash("sha256").update(message).digest("hex").slice(0, 12)})`;
    }));

  it("shows the team tree", () => step("team-tree", "Team tree shows the master, two leads and two sub-agents each",
    "The prompt's team: a native Claude lead with teammates and an app-spawned lead with sub-agents (DEC-104)", async () => {
      await tab("agents");
      let shape = { leads: [] };
      const seen = await waitFor(async () => {
        shape = teamShape(await graph());
        return shape;
      }, TEAM_MS, "the team tree never showed the trial team");
      run.leads = shape.leads;
      return seen;
    }));

  it("answers the agent's question", () => step("question", "Questions panel shows the agent's question; answer it",
    "A For: Me question reaches only the owner (trial 2 check 4); the answer goes back to the asker", async () => {
      await tab("agents");
      const items = "ul[aria-label=\"Open questions\"] li.tg-question";
      await waitFor(async () => {
        const count = (await texts(items)).length;
        return { done: count > 0, seen: `${count} open questions` };
      }, TEAM_MS, "no agent question reached the Questions panel");
      const before = (await texts(items)).length;
      const asked = (await texts(`${items} p.pj-pre`))[0] ?? "";
      await click("(//ul[@aria-label=\"Open questions\"]//li[contains(@class, \"tg-question\")])[1]//button[normalize-space(.)=\"Answer\"]");
      await type("//textarea[@id = //label[starts-with(normalize-space(.), \"Your answer to\")]/@for]", ANSWER);
      await click("button=Send answer");
      await until(async () => (await texts(items)).length < before, 60000, "the answered question stayed open");
      return `question: ${asked.slice(0, 300)}; answered: ${ANSWER}; ${before - 1} still open`;
    }));

  it("shows the merges in Changes", () => step("changes", "Changes shows the merges",
    "Each lead's merged work shows as a change set that lands on trial3-land (Land on, DEC-095)", async () => {
      await tab("changes");
      const rows = "ul[aria-label=\"Change sets\"] > li";
      return waitFor(async () => {
        const all = await texts(rows);
        const words = STATE_WORDS.concat("no state").map((word) => [word, all.filter((row) => (STATE_WORDS.find((state) => row.includes(state)) ?? "no state") === word).length])
          .filter(([, count]) => count > 0).map(([word, count]) => `${word} x${count}`).join(", ");
        return { done: all.length >= 2 && !all.some((row) => row.includes("Agent is updating")), seen: `${all.length} change sets: ${words || "none"}; leads ${run.leads.join(", ") || "unknown"}` };
      }, TEAM_MS, "Changes never showed a settled change set for each lead");
    }));

  it("stops every agent", () => step("stop-all", "Stop all",
    "The owner ends the trial with Stop all; every agent stops (trial 2 check 10)", async () => {
      await tab("agents");
      await click("button=Stop all");
      await until(() => says("dialog[open] h2", "Stop every agent of this project?"), UI_MS, "the Stop all question did not open");
      await click("//dialog[@open]//button[normalize-space(.)=\"Stop all\"]");
      await until(async () => !(await shown("dialog[open]")), 60000, "the Stop all dialog stayed open");
      return "Stop all confirmed";
    }));

  it("shows every agent stopped", () => step("final-tree", "Final tree shows every agent stopped",
    "After Stop all no agent of the project runs", async () => {
      await tab("agents");
      return waitFor(async () => {
        const { nodes } = await graph();
        const running = nodes.filter((node) => !ended(node));
        return { done: nodes.length > 1 && running.length === 0, seen: `${nodes.length} nodes; still running: ${running.map((node) => `${node.label} ${node.status ?? "open"}`).join(", ") || "none"}` };
      }, STOP_MS, "agents were still running after Stop all");
    }));

  it("puts Master input back", () => step("restore-master-input", "Put Master input back",
    "The owner's own Master input choice comes back after the trial", async () => {
      assert(run.input, "needs the Master input read by the master-input step");
      await go("#/settings/profile");
      if (await $(segment("Master input", run.input)).getAttribute("aria-checked") !== "true") await click(segment("Master input", run.input));
      await until(async () => await $(segment("Master input", run.input)).getAttribute("aria-checked") === "true", UI_MS, "Master input did not go back");
      return `Master input reads ${run.input} again`;
    }));
});
