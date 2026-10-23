import assert from "node:assert/strict";
import { join } from "node:path";
import { borrowSignIn, engineProfileHomes, managedCodexHome } from "./v1-flow-auth.mjs";

const source = managedCodexHome(process.env);
const root = process.env.V1_ROOT;
const evidence = process.env.V1_EVIDENCE;
const ledger = join(evidence, "borrowed-sign-in.json");
const ollama = process.env.V1_OLLAMA_URL ?? "http://127.0.0.1:11434/v1";
const ollamaModel = process.env.V1_OLLAMA_MODEL ?? "qwen2.5:0.5b";
const OWNER = "V1 Flow Owner";
const CODEX_ACCOUNT = "Codex managed";
const LOCAL_ACCOUNT = "Local qwen";
const POOL = "V1 flow pool";
const PROJECT = "Habit tracker";
const ABOUT = "A mobile app that tracks daily habits and reminds people to keep their streaks.";
const MESSAGE = "Reply with one short sentence: what should we build first?";
const SETTLED_TURN = /The master stopped|Stopped|Not signed in|Sign-in is out of date|Settings changed/;
const RESEARCH_WORDS = ["Queued", "Needs a pool", "Running", "Done", "Failed"];
let projectId = "";

function page(selector, action = "ready", value = "") {
  return browser.execute((selector, action, value) => {
    const visible = (node) => Boolean(node && node.getClientRects().length);
    const among = (query, match) => [...document.querySelectorAll(query)].find((node) => visible(node) && match(node));
    const node = selector.startsWith("button=") ? among("button, a", (item) => item.textContent.trim() === selector.slice(7))
      : selector.startsWith("text=") ? among("button, a", (item) => item.textContent.includes(selector.slice(5)))
      : selector.startsWith("label=") ? among("label", (item) => item.textContent.includes(selector.slice(6)))
      : among(selector, () => true);
    const usable = Boolean(node && !node.disabled && node.getAttribute("aria-disabled") !== "true");
    if (action === "ready") return usable;
    if (action === "checked") {
      const box = node?.matches("input") ? node : node?.querySelector("input");
      return box ? box.checked : node?.getAttribute("aria-checked") === "true";
    }
    if (!usable) return false;
    if (action === "click") {
      node.click();
      return true;
    }
    if (action === "type") {
      const prototype = node instanceof HTMLInputElement ? HTMLInputElement.prototype
        : node instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : null;
      if (!prototype) return false;
      Object.getOwnPropertyDescriptor(prototype, "value").set.call(node, value);
      node.dispatchEvent(new Event("input", { bubbles: true }));
      return node.value === value;
    }
    return false;
  }, selector, action, value);
}

function text(selector) {
  return browser.execute((selector) => [...document.querySelectorAll(selector)]
    .filter((node) => node.getClientRects().length).map((node) => node.textContent).join("\n"), selector);
}

function hash() {
  return browser.execute(() => location.hash);
}

async function until(condition, timeout, label) {
  try {
    await browser.waitUntil(condition, { timeout, interval: 1000 });
  } catch {
    throw new Error(`${label}; on screen: ${JSON.stringify(await text("[role=alert]"))}; at ${await hash()}`);
  }
}

async function click(selector, timeout = 30000) {
  await until(() => page(selector), timeout, `${selector} never became usable`);
  assert.equal(await page(selector, "click"), true, `${selector} click`);
}

async function type(selector, value) {
  await until(() => page(selector), 30000, `${selector} never became usable`);
  assert.equal(await page(selector, "type", value), true, `${selector} typing`);
}

async function shot(name) {
  await browser.saveScreenshot(join(evidence, `${name}.png`));
}

async function go(route) {
  await browser.execute((route) => { location.hash = route; }, route);
}

async function researchRow() {
  return browser.execute(() => [...document.querySelectorAll(".pj-research-row")].map((row) => ({
    title: row.querySelector(".pj-kind")?.textContent.trim() ?? "",
    status: row.querySelector(".badge")?.textContent.trim() ?? "",
    progress: row.querySelector(".pj-secondary")?.textContent.trim() ?? "",
  })));
}

async function researchSettledIn(words, timeout, label) {
  let rows = [];
  await until(async () => {
    rows = await researchRow();
    return rows.length === 1 && words.includes(rows[0].status);
  }, timeout, `${label}; research rows ${JSON.stringify(rows)}`);
  return rows[0];
}

describe("CrazyAgent V1 flow on the real desktop app and engine", () => {
  it("saves the owner profile on first run", async () => {
    await until(() => page("#profile-name"), 120000, "first-run Welcome page did not appear");
    assert.match(await text("h1"), /Tell CrazyAgent about you/);
    await type("#profile-name", OWNER);
    await type("#profile-role", "Founder");
    await type("#profile-bio", "Builds small apps for everyday routines.");
    await shot("01-first-run");
    await click("button=Save and continue");
    await until(async () => (await text("main")).includes("No projects yet"), 30000, "dashboard did not open after first run");
  });

  it("shows the empty dashboard with the setup steps", async () => {
    const main = await text("main");
    assert.match(main, /Projects/);
    assert.match(main, /No projects yet/);
    assert.match(main, /Get set up/);
    assert.equal(await page("button=Add account"), true);
    assert.equal(await page("button=Create pool"), true);
    await shot("02-dashboard-empty");
  });

  it("adds a managed Codex account from an existing signed-in profile", async () => {
    await click("button=Add account");
    await click("text=ChatGPT / Codex");
    await click("text=Use the Codex sign-in on this Mac");
    await type("#account-name", CODEX_ACCOUNT);
    await click("button=Use this sign-in");
    await until(async () => await page("button=Try again") || await page("button=Check again") ||
      (await text("dialog[open]")).includes("Account added"), 180000, "managed Codex sign-in did not settle");
    if (!(await text("dialog[open]")).includes("Account added")) {
      assert.equal(engineProfileHomes(root).length, 1, "the sign-in made one engine profile home");
      borrowSignIn(source, root, ledger);
      await click(await page("button=Try again") ? "button=Try again" : "button=Check again");
    }
    await until(async () => (await text("dialog[open]")).includes("Account added"), 180000, "managed Codex account did not sign in");
    assert.match(await text("dialog[open] .success"), new RegExp(`${CODEX_ACCOUNT}[\\s\\S]*Signed in`));
    await shot("03-codex-account");
  });

  it("adds a local Ollama account", async () => {
    await click("button=Add another account");
    await click("text=Local models (Ollama)");
    await type("#account-name", LOCAL_ACCOUNT);
    await type("#account-url", ollama);
    await type("#account-model", ollamaModel);
    await click("button=Save");
    await until(async () => /Connected\. \d+ models? found\./.test(await text("dialog[open] .success")), 60000, "local account did not connect");
    await shot("04-local-account");
    await click("button=Done");
    await until(async () => {
      const main = await text("main");
      return main.includes(CODEX_ACCOUNT) && main.includes(LOCAL_ACCOUNT);
    }, 30000, "accounts list did not show both accounts");
    assert.match(await text("main"), /Signed in/);
  });

  it("creates a pool holding both accounts", async () => {
    await go("#/home");
    await click("button=Create pool");
    await type("#pool-name", POOL);
    await click(`label=${CODEX_ACCOUNT}`);
    await click(`label=${LOCAL_ACCOUNT}`);
    assert.equal(await page(`label=${CODEX_ACCOUNT}`, "checked"), true);
    assert.equal(await page(`label=${LOCAL_ACCOUNT}`, "checked"), true);
    await shot("05-pool-editor");
    await click("button=Save pool");
    await until(async () => !(await page("#pool-name")) && (await text("main")).includes(POOL), 30000, "pool was not saved");
  });

  it("creates a project with research picked and the pool assigned", async () => {
    await go("#/home");
    await click("button=New project");
    await type("#np-name", PROJECT);
    await type("#np-about", ABOUT);
    await click("button=Next");
    await until(async () => (await text("h2")).includes("Documents") || await page("#np-doc-path"), 15000, "documents step did not open");
    await click("button=Next");
    await until(() => page("#np-research-market_research"), 15000, "work step did not open");
    for (const kind of ["market_research", "competitor_research", "design_research"]) {
      assert.equal(await page(`#np-research-${kind}`, "checked"), true, `${kind} starts on`);
    }
    await click("#np-research-competitor_research");
    await click("#np-research-design_research");
    assert.equal(await page("#np-research-market_research", "checked"), true);
    assert.equal(await page("#np-research-competitor_research", "checked"), false);
    assert.equal(await page("#np-research-design_research", "checked"), false);
    await shot("06-research-toggles");
    await click("button=Next");
    await click(`label=${POOL}`);
    assert.equal(await page(`label=${POOL}`, "checked"), true);
    await click("button=Next");
    await shot("07-team");
    await click("button=Create project", 60000);
    await until(async () => /^#\/projects\/[^/?]+\/overview/.test(await hash()), 60000, "project page did not open");
    projectId = decodeURIComponent((await hash()).split("/")[2]);
    assert.ok(projectId.length > 0);
  });

  it("shows research progress on the project page", async () => {
    const row = await researchSettledIn(["Running", "Done", "Failed"], 300000, "research never started");
    assert.equal(row.title, "Market research");
    assert.ok(RESEARCH_WORDS.includes(row.status));
    assert.notEqual(row.status, "Failed", `research failed: ${row.progress}`);
    assert.match(await text("main"), /Recent activity/);
    await shot("08-research-progress");
  });

  it("shows the brief in the project context", async () => {
    await click("#tab-context");
    await until(async () => (await text("nav[aria-label='Project folder']")).includes("brief.md"), 30000, "project folder did not list brief.md");
    await click("button=brief.md");
    await until(async () => (await text(".pj-reader")).includes("daily habits"), 30000, "brief did not open in the reader");
    await shot("09-context-brief");
  });

  it("gets a real reply from the master conversation", async () => {
    await click("#tab-conversation");
    await type("[data-testid=master-message]", MESSAGE);
    await click("[data-testid=send-master-message]");
    let turn = "";
    await until(async () => {
      const turns = await browser.execute(() => [...document.querySelectorAll("[data-testid=master-turn]")].map((node) => node.textContent));
      turn = turns.at(-1) ?? "";
      return turns.length > 0 && ((await text("[data-testid=master-reply]")).trim().length > 0 || SETTLED_TURN.test(turn));
    }, 660000, "master turn did not settle");
    await shot("10-master-reply");
    assert.match(turn, new RegExp(MESSAGE.replace(/[.?:]/g, "\\$&")));
    assert.ok((await text("[data-testid=master-reply]")).trim().length > 0, `real master reply required; turn: ${turn}`);
  });

  it("finishes the research and opens its report", async () => {
    await click("#tab-overview");
    const row = await researchSettledIn(["Done", "Failed"], 900000, "research did not finish");
    assert.equal(row.status, "Done", `research failed: ${row.progress}`);
    await click("button=Open report");
    await until(async () => (await hash()).includes("/context?file="), 15000, "report did not open in the context tab");
    await until(async () => (await text(".pj-reader")).includes("Market research"), 30000, "report text did not load");
    await shot("11-research-report");
  });
});
