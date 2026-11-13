import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { handOff, onEngine, startingWith } from "./codex-task-engine.mjs";

/** The probe every run of the stand-in Codex makes before it answers: it writes the arguments
 * the engine started it with, one per line, to its own retry-argv-<pid>.txt in its home. */
const ARGV = String.raw`case "$1" in
  app-server) ;;
  *) for a in "$@"; do printf '%s\n' "$a"; done > "$CODEX_HOME/retry-argv-$$.txt" ;;
esac
`;

const FORM = "form[aria-label=\"When a model provider can't be reached\"]";
const TITLES = `${FORM} label.radio-card .title`;
const SAVE = `${FORM} button[type=submit]`;
const HELP = "New runs take the choice; runs already going keep theirs. Each harness line is proven on read-only runs.";

/** Every option of When a model provider can't be reached (DEC-163), with the Codex settings
 * a sub-agent on the pool's address account then starts with: the engine's custom provider
 * takes the retry counts, and every option but Harness default turns the endless wait off. */
const OPTIONS = [
  {
    choice: "stop_at_once", title: "Stop at once",
    codex: ["features.unbounded_connection_retries=false", "model_providers.crazyagent.request_max_retries=0", "model_providers.crazyagent.stream_max_retries=0"],
  },
  { choice: "retry_briefly", title: "Retry briefly", codex: ["features.unbounded_connection_retries=false"] },
  { choice: "harness_default", title: "Harness default", codex: [] },
];

/** The page read of the checked option and whether Save rests. Output: the script. */
const RESTING = `[document.querySelector(${JSON.stringify(`${FORM} input[type=radio]:checked`)})?.value ?? null,
  document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null]`;

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
async function until(what, wanted, read) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** The retry settings the sub-agent's Codex was started with: each `-c` value about its
 * connection wait or its retry counts, in order, the same in every run that took settings.
 * Input: the engine root. Output: the values, or null before a sub-agent ran. */
function retrySettings(runtime) {
  const agents = join(runtime, "team", "agents");
  if (!existsSync(agents)) return null;
  const homes = readdirSync(agents).map((name) => join(agents, name, "home")).filter((home) => existsSync(home));
  const runs = homes.flatMap((home) => readdirSync(home).filter((name) => /^retry-argv-\d+\.txt$/.test(name))
    .map((name) => readFileSync(join(home, name), "utf8").split("\n")))
    .filter((argv) => argv.includes("-c"));
  if (runs.length === 0) return null;
  const seen = runs.map((argv) => argv.filter((argument, index) => argv[index - 1] === "-c")
    .filter((setting) => setting.startsWith("features.unbounded_connection_retries=") || /_max_retries=/.test(setting)));
  assert.ok(seen.every((settings) => JSON.stringify(settings) === JSON.stringify(seen[0])), `every run took the same settings: ${JSON.stringify(seen)}`);
  return seen[0];
}

/** Opens Settings, Harnesses and waits for the section with its three options and the
 * engine's lines. Inputs: the owner, the page and the served app. Output: none. */
async function harnesses(me, page, site) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/settings/harnesses` });
  await me.waitFor("the When a model provider can't be reached section", (text) => ["When a model provider can't be reached", "What harnesses do",
    ...OPTIONS.map((option) => option.title), HELP, "RCLI: one try. The engine never repeats an RCLI request."].every((words) => text.includes(words)));
}

/** Picks one option and saves it, waiting until the engine keeps it and the page shows it
 * stored with Save resting. Inputs: the bridge, the owner, the page and the option. */
async function save(bridge, me, page, option) {
  await me.click(option.title, TITLES);
  await until(`${option.title} picked on the page`, [option.choice, false], () => page.evaluate(RESTING));
  await me.click("Save changes", SAVE);
  await until(`${option.title} in the engine`, option.choice, async () => (await value(bridge, "desktop_provider_retry")).choice);
  await me.waitFor("the saved note", (text) => text.includes("Model provider setting saved."));
  await until(`${option.title} stored on the page`, [option.choice, true], () => page.evaluate(RESTING));
}

for (const option of OPTIONS) {
  test(`When a model provider can't be reached: ${option.title} is saved from Settings, Harnesses and the next Codex sub-agent starts with its settings`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-retry-${option.choice}-`, async ({ project, runtime, me, page, site, bridge }) => {
      const before = await value(bridge, "desktop_provider_retry");
      assert.equal(before.choice, "retry_briefly", "a fresh engine retries briefly");
      assert.equal(before.notes.length, 9, "a line for every option and harness");
      await harnesses(me, page, site);
      await until("Retry briefly on the page, with Save waiting for a change", ["retry_briefly", true], () => page.evaluate(RESTING));
      if (option.choice === "retry_briefly") await save(bridge, me, page, OPTIONS[0]);
      await save(bridge, me, page, option);
      await me.capture(`provider-retry-${option.choice}`);
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
      await me.unfold("Agents and tasks");
      await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
      await handOff(me, page, project, runtime);
      const settings = retrySettings(runtime);
      assert.ok(settings !== null, "the sub-agent's Codex wrote the arguments it started with");
      assert.deepEqual(settings, option.codex, `${option.title}: the Codex retry settings`);
      t.diagnostic(`${option.title}: ${JSON.stringify(settings)}`);
    }, 1, startingWith(ARGV)));
}
