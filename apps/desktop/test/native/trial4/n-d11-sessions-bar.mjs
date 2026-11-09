import { click, shown, texts } from "../settings-steps.mjs";
import { REPLY_MS, UI_MS, liveLine, masterRows, masterState, messageCount, replyLines, sendToMaster, startIfPaused, terminalLive } from "./n-d8-master-reply.mjs";

/** The message that gives the scratch session one turn, so it is listed and can be deleted. */
export const SCRATCH = "Trial 4 check N-D11 scratch session. Reply with the single word ok.";

const NEW = "[data-testid=new-session]";
const TOGGLE = "[data-testid=sessions-toggle]";
const ITEMS = "li.pj-session-item";
const INPUTS = ["Terminal only", "Message box only", "Both"];

/** Reads how many sessions the Sessions button says the owner can resume. Input: its text.
 * Output: the number, 0 when it names none. */
export function listedCount(label) {
  const found = /\((\d+)\)\s*$/.exec(label.trim());
  return found ? Number(found[1]) : 0;
}

/** Finds the list item of the trial session, the one session that used agents, among the
 * resumable sessions. Input: the items' texts. Output: its index; throws when none or several
 * name agents. */
export function trialItem(items) {
  const named = items.flatMap((item, at) => item.includes("Agents:") ? [at] : []);
  if (named.length !== 1) throw new Error(`expected one resumable session that used agents, found ${named.length} of ${items.length}`);
  return named[0];
}

/** Finds the newest resumable session that used no agents: the scratch session. Input: the
 * items' texts, newest first. Output: its index; throws when there is none. */
export function scratchItem(items) {
  const at = items.findIndex((item) => !item.includes("Agents:"));
  if (at < 0) throw new Error(`no resumable session without agents among ${items.length}`);
  return at;
}

/** Opens the Sessions list when it is closed. */
async function openList() {
  if (await $(TOGGLE).getAttribute("aria-expanded") !== "true") await click(TOGGLE);
}

/** Reads the resumable sessions the open list shows. Output: their texts, newest first. */
function items() {
  return texts(ITEMS);
}

/** Sets the Terminal input setting in Settings, Profile and waits until it reads back. Inputs:
 * the kit and the option. Output: the option it had before. */
async function input(kit, option) {
  await kit.go("#/settings/profile");
  let was = null;
  await kit.until(async () => {
    for (const choice of INPUTS) if (await shown(kit.segment("Terminal input", choice)) && await $(kit.segment("Terminal input", choice)).getAttribute("aria-checked") === "true") was = choice;
    return was !== null;
  }, UI_MS, "Terminal input was never shown with a choice");
  if (was !== option) await click(kit.segment("Terminal input", option));
  await kit.until(async () => await $(kit.segment("Terminal input", option)).getAttribute("aria-checked") === "true", UI_MS, `Terminal input did not take ${option}`);
  return was;
}

export const checks = [{
  id: "N-D11", op: ["OP-10"], title: "Sessions bar: an old session continues, a second New session adds nothing, Delete removes a parked session",
  why: "owner: \"Old sessions do not reopen and continue. Blank sessions can be added without limit and none can be deleted\"",
  after: "end",
  run: async (kit) => {
    const was = await input(kit, "Both");
    try {
      await kit.tab("conversation");
      const started = await startIfPaused(kit, kit.project());
      await terminalLive(kit, REPLY_MS);
      const trial = messageCount(await liveLine());
      if (!trial) throw new Error(`the trial session should be live with messages; the Sessions bar reads "${await liveLine()}"`);
      const before = listedCount(await $(TOGGLE).getText());
      await click(NEW);
      if (await shown("[data-testid=confirm-switch]")) await click("[data-testid=confirm-switch]");
      await kit.until(async () => messageCount(await liveLine()) === 0, UI_MS, "New session did not open a session with no messages");
      await kit.until(async () => listedCount(await $(TOGGLE).getText()) === before + 1, UI_MS, `New session did not park the trial session (Sessions was ${before})`);
      await kit.until(async () => !(await $(NEW).isEnabled()), UI_MS, "New session stayed on in a session with no messages");
      const why = await $(NEW).getAttribute("title");
      await browser.execute((selector) => document.querySelector(selector).click(), NEW);
      await kit.reopen("conversation");
      await kit.until(async () => messageCount(await liveLine()) === 0, UI_MS, "the live session changed after the second New session");
      const after = listedCount(await $(TOGGLE).getText());
      if (after !== before + 1) throw new Error(`the second New session changed the Sessions list from ${before + 1} to ${after}`);
      await kit.screen();
      await terminalLive(kit, REPLY_MS);
      const sent = await sendToMaster(kit, SCRATCH);
      const { master } = masterState((await kit.graph()).nodes);
      await kit.waitFor(async () => {
        const count = replyLines(await masterRows(), master?.harness ?? "claude-code");
        return { done: count > 0, seen: `${count} reply lines in the scratch session` };
      }, REPLY_MS, "the master did not answer in the scratch session");
      await openList();
      await kit.until(async () => (await items()).length === before + 1, UI_MS, "the Sessions list did not show the parked trial session");
      const resume = trialItem(await items());
      await click(`(//li[contains(@class, "pj-session-item")])[${resume + 1}]//button[@data-testid="resume-session"]`);
      if (await shown("[data-testid=confirm-switch]")) await click("[data-testid=confirm-switch]");
      await kit.until(async () => messageCount(await liveLine()) === trial, UI_MS, `the resumed session did not read ${trial} messages`);
      await terminalLive(kit, REPLY_MS);
      await kit.screen();
      await openList();
      await kit.until(async () => (await items()).length === before + 1, UI_MS, "the Sessions list did not show the parked scratch session");
      const listed = (await items()).length;
      const gone = scratchItem(await items());
      await click(`(//li[contains(@class, "pj-session-item")])[${gone + 1}]//button[@data-testid="delete-session"]`);
      await kit.until(() => shown("[data-testid=confirm-delete]"), UI_MS, "Delete did not ask to confirm");
      if ((await items()).length !== listed) throw new Error("the session left the list before the owner confirmed");
      await click("[data-testid=confirm-delete]");
      await kit.until(async () => (await items()).length === listed - 1, UI_MS, "the deleted session stayed in the list");
      if (messageCount(await liveLine()) !== trial) throw new Error("Delete changed the live session");
      await kit.screen();
      return `${started ? "Start all; " : ""}trial session with ${trial} messages; New session parked it (Sessions ${before} to ${before + 1}); the second New session was off ("${why}") and changed nothing; scratch message sent by ${sent}; resumed the trial session with ${trial} messages in the live terminal; deleted the parked scratch session after its confirm (${listed} to ${listed - 1} listed)`;
    } finally {
      await input(kit, was);
    }
  },
}];
