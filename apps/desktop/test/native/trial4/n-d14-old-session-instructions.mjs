import { randomBytes } from "node:crypto";
import { click, shown, text } from "../settings-steps.mjs";
import { REPLY_MS, UI_MS, liveLine, masterRows, messageCount, sendToMaster, startIfPaused, terminalLive } from "./n-d8-master-reply.mjs";

/** The trial 4 run 3 project, whose master session the N-D14 record step recorded on build 18.14. */
export const OLD_PROJECT = "ca-trial-4c";

/** The words that open the line this check adds to the project's instructions. */
export const MARK = "Trial 4 marker line:";

/** The question that asks the master to quote the added line without giving its token. */
export const QUESTION = `Quote, word for word, the line of your project instructions that starts with "${MARK}".`;

const EDITOR = "#process-project";
const SAVE = "//form[.//*[@id=\"process-project\"]]//button[normalize-space(.)=\"Save\"]";
const STOP = "//dialog[@open]//button[normalize-space(.)=\"Stop all\"]";

/** Adds one marked line to an instruction set, after its text and a blank line. Inputs: the
 * saved text and the token. Output: { line, edited }. */
export function addMarkedLine(saved, token) {
  if (!/^[a-z0-9-]+$/.test(token)) throw new Error("the token is lowercase letters, digits and dashes");
  const line = `${MARK} ${token}`;
  const body = saved.trimEnd();
  return { line, edited: body === "" ? `${line}\n` : `${body}\n\n${line}\n` };
}

/** Finds a project id by its name in the app's project list. Input: the name. Output: the id;
 * throws when the app lists no project of that name. */
async function projectId(name) {
  const answer = await browser.executeAsync((done) => {
    window.__TAURI_INTERNALS__.invoke("desktop_project_list").then(done, (failure) => done({ failed: String(failure) }));
  });
  const found = (answer?.items ?? answer?.value?.items ?? []).filter((item) => item.name === name);
  if (found.length !== 1) throw new Error(`the app lists ${found.length} projects named ${name}`);
  return found[0].id;
}

/** Saves an instruction set in the open project Process tab and waits until it is saved.
 * Inputs: the kit and the text. */
async function saveInstructions(kit, value) {
  await kit.paste(EDITOR, value);
  await click(SAVE);
  await kit.until(async () => !(await text("form")).includes("Not saved yet."), UI_MS, "the project instructions were not saved");
}

export const checks = [{
  id: "N-D14", op: ["OP-2", "OP-4", "OP-33"], title: "A master session recorded on 18.14 resumes on 18.15 and reads an edited instruction set",
  why: "owner: \"Agents and the master work without the app and project instruction set. The owner cannot see that it was loaded\"",
  after: "open-app",
  run: async (kit) => {
    const id = await projectId(OLD_PROJECT);
    const base = `#/projects/${encodeURIComponent(id)}`;
    await kit.go(`${base}/process`);
    await kit.until(() => shown(EDITOR), UI_MS, `the ${OLD_PROJECT} instructions editor was never shown`);
    const saved = await $(EDITOR).getValue();
    const token = `t4-nd14-${randomBytes(4).toString("hex")}`;
    const { line, edited } = addMarkedLine(saved, token);
    let started = false;
    try {
      await saveInstructions(kit, edited);
      await kit.go(`${base}/conversation`);
      started = await startIfPaused(kit, id);
      await terminalLive(kit, REPLY_MS);
      let turns = messageCount(await liveLine());
      let resumed = "it was the live session";
      if (!turns) {
        await click("[data-testid=sessions-toggle]");
        await click("(//button[@data-testid=\"resume-session\"])[1]");
        await kit.until(async () => (messageCount(await liveLine()) ?? 0) > 0, UI_MS, `no recorded ${OLD_PROJECT} session resumed`);
        await terminalLive(kit, REPLY_MS);
        turns = messageCount(await liveLine());
        resumed = "resumed from the Sessions list";
      }
      const session = await liveLine();
      const sent = await sendToMaster(kit, QUESTION);
      await kit.waitFor(async () => {
        const quoted = (await masterRows()).some((row) => row.includes(token));
        return { done: quoted, seen: quoted ? "the master quoted the added line" : "the added line's token is not on the master terminal" };
      }, REPLY_MS, "the master did not quote the edited instruction line");
      await kit.screen();
      return `${OLD_PROJECT} session with ${turns} messages (${resumed}; ${session.trim()}); added "${line}" to its project instructions; asked by ${sent}; the master quoted the line's token`;
    } finally {
      await kit.go(`${base}/process`);
      await kit.until(() => shown(EDITOR), UI_MS, `the ${OLD_PROJECT} instructions editor was never shown again`);
      if (await $(EDITOR).getValue() !== saved) await saveInstructions(kit, saved);
      if (started) {
        await kit.go(`${base}/agents`);
        await click("button=Stop all");
        await click(STOP);
        await kit.until(async () => !(await shown("dialog[open]")), 60000, "the Stop all dialog stayed open");
      }
    }
  },
}];
