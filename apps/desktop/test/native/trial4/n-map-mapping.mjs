import { CANARY, DECOY, RESEARCH_DOCS } from "../../../../../scripts/trial/mapping-docs.mjs";
import { click, says, shown } from "../settings-steps.mjs";

/** The setup step whose row the mapping checks read. */
export const MAP_STEP = "Map the project";

/** The longest the mapping may show only "Working" before its row names the try. */
export const PROGRESS_MS = 60000;

/** The longest the mapping may take before it ends on its own. */
export const MAPPING_MS = 15 * 60000;

const POOL = process.env.TRIAL_POOL ?? "";
const START = "//dialog[@open]//button[normalize-space(.)=\"Start setup\"]";
const REVIEW = ".pj-switch-item";
const SETUP_ROUTE = /^#\/projects\/([^/]+)\/setup$/;
const RUNNING = ["Working", "Waiting"];
const PAUSED = /^Rate limited until /;
const LINE = /^Mapping, try ([1-9][0-9]*) of ([1-9][0-9]*), (?:under 1 min|([0-9]+) min)\./;
const IN_REPO = "Already in the repo: ";
const DECOY_MARK = DECOY.split("/").pop().replace(/\.md$/, "").toLowerCase();
const NEEDED = [
  { kind: "competitor_research", label: "Competitor research", file: RESEARCH_DOCS.competitor },
  { kind: "market_research", label: "Market research", file: RESEARCH_DOCS.market },
];

/** Where "never read" is proven: the app has no read command for the mapping run's prompt or
 * transcript, which stay in the engine's managed agent home, so MAP-2 proves only that the decoy
 * is never listed or shown, and this engine unit test that its text never reaches the prompt. */
export const NEVER_READ = "crates/crazyagent/src/onboarding/tests.rs secret_named_documents_are_neither_read_nor_listed (7a8f4d2e, fix/mapping-reads-content)";

/** What MAP-1 leaves for MAP-2: the id of the project whose setup it started, the window
 * texts its screenshots read and, when the project's pool was still paused at the end, why. */
const seen = { id: null, screens: [], blocked: null };

/** Reads a mapping progress line. Input: the row's line or null. Output: { number, tries,
 * minutes }, or null when the line names no try. */
export function tryOf(line) {
  const match = LINE.exec(line ?? "");
  if (!match) return null;
  const [number, tries] = [Number(match[1]), Number(match[2])];
  if (number > tries) return null;
  return { number, tries, minutes: match[3] === undefined ? 0 : Number(match[3]) };
}

/** Tells whether the mapping is still under way. Input: the row's badge word. Output: true
 * while it works, waits or its pool is paused ("Rate limited until <time>"). */
export function stillRunning(word) {
  return RUNNING.includes(word) || PAUSED.test(word);
}

/** Tells whether the mapping row showed only "Working" too long. Inputs: the reads of the row
 * in time order, { ms, line, word }, and whether the mapping has ended. Output: the fault, or
 * null: Working with no try line for over PROGRESS_MS since it first read Working or since its
 * pool was last paused, or, once ended, no try line ever. */
export function progressFault(samples, ended = false) {
  let since = null;
  for (const sample of samples) {
    if (PAUSED.test(sample.word)) {
      since = null;
      continue;
    }
    if (since === null && sample.word !== "Working") continue;
    since ??= sample.ms;
    const gap = sample.ms - since;
    if (gap > PROGRESS_MS) return `the mapping showed only "Working" for ${Math.round(gap / 1000)} s with no try line`;
    if (tryOf(sample.line)) break;
  }
  return ended && !samples.some((sample) => tryOf(sample.line)) ? "the mapping never showed its progress line" : null;
}

/** Tells whether the mapping failed to end on its own in time. Input: the last read of its row,
 * { ms, line, word }. Output: the fault, or null when it reads Done within MAPPING_MS; a pool
 * still paused is a BLOCKED reason, not a fault of the mapping. */
export function endFault(sample) {
  const seconds = Math.round(sample.ms / 1000);
  if (PAUSED.test(sample.word)) return `BLOCKED: the project's pool was still paused after ${seconds} s (${sample.word})`;
  if (RUNNING.includes(sample.word)) return `the mapping still read ${sample.word} after ${seconds} s`;
  if (sample.word !== "Done") return `the mapping ended ${sample.word} after ${seconds} s${sample.line ? `: ${sample.line}` : ""}`;
  return sample.ms > MAPPING_MS ? `the mapping ended Done after ${seconds} s, over the ${MAPPING_MS / 1000} s it may take` : null;
}

/** Lists the repository files the review's research switches name. Input: the switches,
 * [{ label, notes }]. Output: label to files, empty when a switch names none. */
export function repoFiles(items) {
  const files = {};
  for (const item of items) {
    const note = item.notes.find((text) => text.startsWith(IN_REPO));
    files[item.label] = note ? note.slice(IN_REPO.length).split(",").map((path) => path.trim()).filter(Boolean) : [];
  }
  return files;
}

/** Names the seeded documents the review does not show under their research kind. Input:
 * label to files. Output: "<label>: <file>" for each one missing. */
export function missingInReview(files) {
  return NEEDED.filter((need) => !(files[need.label] ?? []).includes(need.file)).map((need) => `${need.label}: ${need.file}`);
}

/** Reads the research the mapping found from the app's desktop_onboarding answer. Inputs: the
 * answer and the project id. Output: kind to files; throws when the read failed, is another
 * project's or holds no mapping facts, so the check never passes without the read. */
export function rowResearch(answer, id) {
  if (answer?.failed !== undefined) throw new Error(`desktop_onboarding failed: ${answer.failed}`);
  if (answer?.project_id !== id) throw new Error(`desktop_onboarding answered for ${JSON.stringify(answer?.project_id)}, not ${id}`);
  if (typeof answer.facts !== "object" || answer.facts === null) throw new Error(`desktop_onboarding answered no mapping facts for ${id}`);
  const research = {};
  for (const found of answer.facts.research ?? []) research[found.kind] = [...found.files];
  return research;
}

/** Names the seeded documents the onboarding row does not hold under their research kind.
 * Input: kind to files. Output: "<kind>: <file>" for each one missing. */
export function missingInRow(research) {
  return NEEDED.filter((need) => !(research[need.kind] ?? []).includes(need.file)).map((need) => `${need.kind}: ${need.file}`);
}

/** Finds the decoy in a text: its file name or its canary, in any case. Input: the text.
 * Output: DECOY and or CANARY for each one the text holds. */
export function leaks(text) {
  const lower = text.toLowerCase();
  return [[DECOY, DECOY_MARK], [CANARY, CANARY.toLowerCase()]].filter(([, mark]) => lower.includes(mark)).map(([name]) => name);
}

/** Reads the mapping step's row on the setup page. Output: { title, line, word }, or null while
 * the page shows no such row. */
async function mappingRow() {
  return browser.execute((title) => {
    const row = [...document.querySelectorAll("ol[aria-label=\"Setup steps\"] > li")].find((item) => item.querySelector("strong")?.textContent.trim() === title);
    if (!row) return null;
    return { title, line: row.querySelector(".in-row-main .pj-small")?.textContent.trim() || null, word: row.querySelector(".badge")?.textContent.trim() ?? "" };
  }, MAP_STEP);
}

/** Reads the research switches of the setup review. Output: [{ label, notes }] in screen order. */
async function switchItems() {
  return browser.execute((selector) => [...document.querySelectorAll(selector)].map((item) => ({
    label: item.querySelector("strong")?.textContent.trim() ?? "",
    notes: [...item.querySelectorAll("span.pj-secondary")].map((note) => note.textContent.trim()),
  })), REVIEW);
}

/** Calls one of the app's own commands from the page. Inputs: the command and its arguments.
 * Output: the answer, or { failed } when the app refused it. */
async function command(name, args) {
  return browser.executeAsync((name, args, done) => {
    window.__TAURI_INTERNALS__.invoke(name, args).then(done, (failure) => done({ failed: String(failure) }));
  }, name, args);
}

export const checks = [{
  id: "MAP-1", op: ["MAP-1"], title: "The project mapping shows its try and minutes, and ends on its own within 15 minutes",
  why: "owner: \"There should be progress shown for the mapping ... I should not be stuck\"",
  after: "open-app",
  run: async (kit) => {
    seen.id = null;
    seen.screens = [];
    seen.blocked = null;
    await kit.go("#/home");
    const row = `//li[contains(concat(" ", normalize-space(@class), " "), " in-row ")][.//strong[normalize-space(.)=${kit.lit(kit.NAME)}]]`;
    await kit.until(() => shown(row), 60000, `Home never listed ${kit.NAME} in the projects folder`);
    const action = (await $(`${row}//button`).getText()).trim();
    if (action !== "Set up this project") throw new Error(`MAP-1 needs ${kit.NAME} not set up yet, but Home offers ${action}`);
    await click(`${row}//button`);
    await kit.until(() => says("dialog[open] h2", `Set up ${kit.NAME}`), kit.UI_MS, "the Set up dialog did not open");
    if (POOL) await click(`//dialog[@open]//label[.//*[normalize-space(.)=${kit.lit(POOL)}]]`);
    await click(START, 60000);
    const started = Date.now();
    let route = "";
    await kit.until(async () => SETUP_ROUTE.test(route = await kit.hash()), kit.UI_MS, "the setup page did not open");
    seen.id = decodeURIComponent(SETUP_ROUTE.exec(route)[1]);
    const samples = [];
    let progress = null;
    let shot = null;
    try {
      await kit.waitFor(async () => {
        const read = await mappingRow();
        if (!read) return { done: false, seen: `no ${MAP_STEP} row on the setup page` };
        const sample = { ms: Date.now() - started, ...read };
        samples.push(sample);
        progress ??= progressFault(samples);
        if (!shot && read.word === "Working" && tryOf(read.line)) {
          shot = sample;
          seen.screens.push(await kit.screen());
        }
        return { done: !stillRunning(read.word), seen: `${read.word}${read.line ? `: ${read.line}` : ""} at ${Math.round(sample.ms / 1000)} s` };
      }, MAPPING_MS, `the ${MAP_STEP} step did not end on its own`);
    } catch (error) {
      const paused = samples.at(-1);
      if (!progress && paused && PAUSED.test(paused.word)) throw new Error(seen.blocked = endFault(paused));
      throw new Error([progress, error.message].filter(Boolean).join("; "));
    }
    const last = samples.at(-1);
    progress ??= progressFault(samples, true);
    const end = endFault(last);
    if (progress || end) throw new Error([progress, end].filter(Boolean).join("; "));
    await kit.until(() => shown(REVIEW), kit.UI_MS, "the setup review was not shown after the mapping ended");
    seen.screens.push(await kit.screen());
    const working = samples.find((sample) => sample.word === "Working");
    const tries = tryOf(shot.line).tries;
    const reached = Math.max(...samples.map((sample) => tryOf(sample.line)?.number ?? 0));
    return `set up ${kit.NAME} (${seen.id}); "${shot.line}" ${Math.round((shot.ms - working.ms) / 1000)} s after Working; Done after ${Math.round(last.ms / 1000)} s on try ${reached} of ${tries}; ${samples.length} reads of the row`;
  },
}, {
  id: "MAP-2", op: ["MAP-2"], title: "The project mapping reads the project's research documents and never lists or shows the secret-named decoy",
  why: "owner: \"I already have the context in the project about the project and competitor analysis and market analysis ... it doesn't analyze or map them\"",
  after: "open-app",
  run: async (kit) => {
    try {
      if (seen.blocked) throw new Error(`BLOCKED: no review to read, ${seen.blocked.replace(/^BLOCKED: /, "")}`);
      if (!seen.id) throw new Error("MAP-1 did not start the setup, so there is no review to read");
      const id = seen.id;
      const route = `#/projects/${encodeURIComponent(id)}/setup`;
      if (await kit.hash() !== route) await kit.go(route);
      await kit.until(() => shown(REVIEW), kit.UI_MS, "the setup review was not shown");
      const files = repoFiles(await switchItems());
      await $(REVIEW).scrollIntoView({ block: "start" });
      const screens = [...seen.screens, await kit.screen()];
      const answer = await command("desktop_onboarding", { id });
      const research = rowResearch(answer, id);
      const faults = [
        ...missingInReview(files).map((missing) => `the review does not show ${missing} as already in the repo`),
        ...missingInRow(research).map((missing) => `the onboarding row does not hold ${missing}`),
        ...[...new Set(screens.flatMap(leaks))].map((leak) => `the window shows ${leak}`),
        ...leaks(JSON.stringify(answer)).map((leak) => `the onboarding row holds ${leak}`),
      ];
      if (faults.length > 0) throw new Error(faults.join("; "));
      const named = Object.entries(files).filter(([, paths]) => paths.length > 0).map(([label, paths]) => `${label}: ${paths.join(", ")}`);
      return `review shows Already in the repo for ${named.join("; ")}; the onboarding row holds ${Object.keys(research).join(", ")}; neither ${DECOY} nor its canary listed or shown in ${screens.length} window reads or the onboarding row (never read: ${NEVER_READ})`;
    } finally {
      await kit.go("#/home");
    }
  },
}];
