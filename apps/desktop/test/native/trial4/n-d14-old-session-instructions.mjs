import { randomBytes } from "node:crypto";
import { click, shown, text } from "../settings-steps.mjs";
import { REPLY_MS, UI_MS, liveLine, masterRows, messageCount, sendToMaster, startIfPaused, terminalLive } from "./n-d8-master-reply.mjs";

/** The trial 4 run 3 project, whose master session the N-D14 record step recorded on build 18.14. */
export const OLD_PROJECT = "ca-trial-4c";

/** The master session the N-D14 record step wrote in OLD_PROJECT on build 18.14, as the engine
 * database held it after the record. */
export const RECORDED_SESSION = "master-215080c51594223929c2c5f79d08b6ef09e86c82b1c8bba4a1d895cc1f91da33";

/** The engine's note when a resumed session goes on in a new harness conversation started from a
 * summary: master_sessions.rs FRESH_NOTE. */
export const FRESH_NOTE = "This session continues in a new harness conversation, started with a summary of its earlier turns.";

/** The engine's note when a resumed session's earlier harness conversation could not be read:
 * master_sessions.rs LOST_NOTE. */
export const LOST_NOTE = "This session continues in a new harness conversation. Its earlier harness conversation could not be read, so only the session's state was passed on.";

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

/** Reads the live master session from the app's desktop_master_state answer. Inputs: the answer
 * and the project id. Output: the session id; throws when the read failed, is another project's
 * or names no live session, so the check never passes without the read. */
export function liveSessionOf(answer, id) {
  if (answer?.failed !== undefined) throw new Error(`desktop_master_state failed: ${answer.failed}`);
  if (answer?.project_id !== id) throw new Error(`desktop_master_state answered for ${JSON.stringify(answer?.project_id)}, not ${id}`);
  if (answer.session?.phase !== "configured") throw new Error(`${id} has no live master session`);
  return answer.session.session_id;
}

/** Lists the resume notes in the app's desktop_team_status answer: the engine posts one when a
 * session goes on in a new harness conversation. Inputs: the answer and the project id. Output:
 * the notes' texts; throws when the read failed, is another project's or holds no messages. */
export function resumeNotes(answer, id) {
  if (answer?.failed !== undefined) throw new Error(`desktop_team_status failed: ${answer.failed}`);
  if (answer?.project_id !== id) throw new Error(`desktop_team_status answered for ${JSON.stringify(answer?.project_id)}, not ${id}`);
  if (!Array.isArray(answer.messages)) throw new Error(`desktop_team_status answered no messages for ${id}`);
  return answer.messages.filter((message) => message.state === "note" && (message.text === FRESH_NOTE || message.text === LOST_NOTE)).map((message) => message.text);
}

/** Calls one of the app's own commands from the page. Inputs: the command and its arguments.
 * Output: the answer, or { failed } when the app refused it. */
async function command(name, args) {
  return browser.executeAsync((name, args, done) => {
    window.__TAURI_INTERNALS__.invoke(name, args).then(done, (failure) => done({ failed: String(failure) }));
  }, name, args);
}

/** Reads a project's live master session through the app's commands, opening the project as its
 * Sessions bar does; that takes the window's selection until the window opens the project again.
 * Input: the project id. Output: the session id. */
async function liveSession(id) {
  const opened = await command("desktop_open_project", { id });
  if (opened?.failed !== undefined || typeof opened?.ticket !== "string") throw new Error(`desktop_open_project answered no ticket for ${id}: ${opened?.failed ?? "no failure"}`);
  return liveSessionOf(await command("desktop_master_state", { ticket: opened.ticket }), id);
}

/** Fails when the engine noted that a project's master session went on in a new harness
 * conversation. Input: the project id. */
async function noResumeNote(id) {
  const notes = resumeNotes(await command("desktop_team_status", { id }), id);
  if (notes.length > 0) throw new Error(`the ${OLD_PROJECT} session went on in a new harness conversation: ${notes.join(" ")}`);
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
      const live = await liveSession(id);
      if (live !== RECORDED_SESSION) throw new Error(`the live ${OLD_PROJECT} session is ${live}, not ${RECORDED_SESSION}, which the 18.14 record step wrote`);
      await noResumeNote(id);
      await kit.go(`${base}/process`);
      await kit.until(() => shown(EDITOR), UI_MS, `the ${OLD_PROJECT} instructions editor was never shown`);
      await kit.go(`${base}/conversation`);
      await terminalLive(kit, REPLY_MS);
      await kit.until(async () => (messageCount(await liveLine()) ?? 0) > 0, UI_MS, `the ${OLD_PROJECT} master area did not read its sessions again after the check opened the project`);
      const session = await liveLine();
      const sent = await sendToMaster(kit, QUESTION);
      await kit.waitFor(async () => {
        const quoted = (await masterRows()).some((row) => row.includes(token));
        return { done: quoted, seen: quoted ? "the master quoted the added line" : "the added line's token is not on the master terminal" };
      }, REPLY_MS, "the master did not quote the edited instruction line");
      await noResumeNote(id);
      await kit.screen();
      return `${OLD_PROJECT} session ${live.slice(0, 15)}, the one recorded on 18.14, in its own harness conversation, with ${turns} messages (${resumed}; ${session.trim()}); added "${line}" to its project instructions; asked by ${sent}; the master quoted the line's token`;
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
