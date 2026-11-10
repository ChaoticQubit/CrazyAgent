import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import * as kit from "./trial-kit.mjs";
import { CHECKS, OWNER_MS, TUI_CHECKS, loadChecks, plan, runsIn } from "./trial4-checks.mjs";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";
import { startTui } from "../../../../scripts/trial/tui-kit.mjs";

assert(["live", "fixture"].includes(kit.MODE), "TRIAL3_MODE is live or fixture");
const slugs = TRIAL3_STEPS.map((step) => step.slug);
const desktopChecks = await loadChecks(CHECKS, slugs);
const tuiChecks = (await loadChecks(TUI_CHECKS, slugs)).map((check) => ({ ...check, surface: "tui" }));
const checks = [...desktopChecks, ...tuiChecks];
const checkKit = { ...kit, screen: kit.proof };
const LABEL = /^[a-z0-9-]{1,40}$/;

/** Runs one trial 4 check as a step of its own, so a failure is recorded and the next test
 * still runs. Inputs: the check and the kit it drives. Output: its detail; throws when it
 * failed or returned none. */
async function verify(check, using) {
  const detail = await check.run(using);
  assert(typeof detail === "string" && detail.length > 0, `${check.id} returns its detail as text`);
  return detail;
}

/** The command that starts the crazyagent TUI on the trial's engine: the fixture engine's
 * own binary in a fixture run, the app's binary (TRIAL_CLI, from run.mjs) in a live run.
 * Output: the argv; throws when the binary is missing. */
function tuiArgv() {
  const cli = kit.MODE === "fixture" ? process.env.TRIAL3_ENGINE : process.env.TRIAL_CLI;
  assert(cli && isAbsolute(cli) && existsSync(cli), `a TUI check needs the crazyagent binary (${kit.MODE === "fixture" ? "TRIAL3_ENGINE" : "TRIAL_CLI"}), got ${cli}`);
  return [cli, "tui", process.env.CRAZYAGENT_DESKTOP_SOCKET];
}

/** The TUI's environment, the same home and temp folder the app runs with. Output: the
 * environment object. */
function tuiEnv() {
  const base = { PATH: process.env.PATH ?? "/usr/bin:/bin", LANG: "en_US.UTF-8" };
  const fixture = process.env.TRIAL3_FIXTURE_ROOT;
  return kit.MODE === "fixture" ? { ...base, HOME: join(fixture, "home"), TMPDIR: join(fixture, "tmp") } : { ...base, HOME: process.env.HOME };
}

/** Saves a masked TUI frame at the moment that proves a check, named after the running step
 * (NN-<slug>-<k>-<label>.txt) and listed in that step's evidence. Inputs: the TUI kit and an
 * optional label. Output: the masked pane text. */
function tuiProof(tui, label) {
  assert(kit.run.current, "screen runs inside a step");
  assert(label === undefined || LABEL.test(label), `a screen label is a file stem, got ${label}`);
  const k = kit.run.current.shots.length + 1;
  assert(k <= 9, "a step takes at most 9 proof frames");
  const frame = tui.capture([kit.run.current.name.slice(0, 38), k, label].filter((part) => part !== undefined).join("-"));
  kit.run.current.shots.push(frame.path);
  return frame.text;
}

/** Runs one TUI check as a step: starts the crazyagent TUI in a tmux server of its own, runs
 * the check with the TUI kit, saves the last frame and ends the TUI, whatever the check did.
 * Inputs: the step slug, the step title and the check. Throws when the check failed. */
async function verifyTui(slug, title, check) {
  let tui = null;
  try {
    await kit.step(slug, title, check.why, async () => {
      tui = await startTui({ argv: tuiArgv(), env: tuiEnv(), evidence: kit.EVIDENCE, project: kit.NAME, waitMs: kit.UI_MS });
      return verify(check, { ...tui, screen: (label) => tuiProof(tui, label) });
    }, { surface: "tui", shoot: (name) => {
      assert(tui, "the TUI never started");
      return tui.capture(name).path;
    } });
  } finally {
    await tui?.stop();
  }
}

describe(`CrazyAgent trial 4: trial 3 plus ${desktopChecks.length} desktop and ${tuiChecks.length} TUI owner problem checks (${kit.MODE})`, () => {
  before(() => kit.terminalTyping(browser));
  for (const { slug, step, check } of plan(TRIAL3_STEPS, checks)) {
    if (step) {
      it(step.it, () => kit.step(slug, step.title, step.why, step.action));
      continue;
    }
    const title = `${check.id} (${check.op.join(", ")}) ${check.title}`;
    if (!runsIn(check, kit.MODE, OWNER_MS)) it.skip(`${title} (not taken on this ${kit.MODE} run)`);
    else if (check.surface === "tui") it(title, () => verifyTui(slug, title, check));
    else it(title, () => kit.step(slug, title, check.why, () => verify(check, checkKit)));
  }
});
