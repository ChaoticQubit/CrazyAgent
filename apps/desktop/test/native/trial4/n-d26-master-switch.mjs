import { click, shown, toasts } from "../settings-steps.mjs";
import { REPLY_MS, UI_MS, liveLine, masterRows, messageCount, startIfPaused } from "./n-d8-master-reply.mjs";

/** The master harness switches the owner makes, in order: to Codex and back to Claude Code. */
export const SWITCHES = [{ id: "codex", name: "Codex" }, { id: "claude-code", name: "Claude Code" }];

const SELECT = "#ps-master-harness";

/** Reads what the master area shows after a switch: the switch divider names the new
 * harness, the area is headed by it, its terminal has text, it holds at most one message box
 * and no chat console, and the live session kept its messages. Inputs: the area as read, the
 * harness name and the message count before the switches. Output: { done, ok, seen }. */
export function switchSeen(area, name, messages) {
  if (area.console) return { done: true, ok: false, seen: "the master area shows the chat console beside the terminal" };
  if (area.boxes > 1) return { done: true, ok: false, seen: `the master area shows ${area.boxes} message boxes` };
  if (!area.divider.includes(name)) return { done: false, ok: false, seen: `the switch divider reads "${area.divider}", not ${name}` };
  if (!area.title.includes(name)) return { done: false, ok: false, seen: `the master area is headed "${area.title}", not ${name}` };
  if (area.rows === 0) return { done: false, ok: false, seen: `${name}'s terminal has no text yet` };
  if (area.messages === null || area.messages < messages) return { done: false, ok: false, seen: `the live session reads ${area.messages ?? "none"} messages, not the ${messages} it had` };
  return { done: true, ok: true, seen: `divider "${area.divider}", ${area.boxes} message box, ${area.messages} messages carried over` };
}

/** Reads the master area as the owner sees it. Output: { console, boxes, divider, title,
 * rows, messages }. */
async function area() {
  const seen = await browser.execute(() => {
    const visible = (node) => node.getClientRects().length > 0;
    return {
      console: Boolean(document.querySelector(".pj-convo")),
      boxes: [...document.querySelectorAll(".pj-master-live textarea.tg-composer-input")].filter(visible).length,
      divider: document.querySelector("[data-testid=harness-switch]")?.textContent ?? "",
      title: document.querySelector(".pj-master-live .pj-console-title")?.textContent ?? "",
    };
  });
  const rows = (await masterRows()).filter((row) => row.trim() !== "").length;
  return { ...seen, rows, messages: messageCount(await liveLine()) };
}

/** Chooses the project's master harness on its Team settings and saves it, then reads it back.
 * Inputs: the kit and the harness id. */
async function chooseMaster(kit, harness) {
  const route = `#/projects/${encodeURIComponent(kit.project())}/settings?section=team`;
  await kit.go(route);
  await kit.pick(SELECT, harness);
  await click("button=Save team");
  await kit.until(async () => (await toasts()).some((line) => line.includes("Team saved.")), UI_MS, `saving ${harness} as the master harness showed no Team saved.`);
  await kit.tab("overview");
  await kit.go(route);
  await kit.until(async () => await shown(SELECT) && await $(SELECT).getValue() === harness, UI_MS, `the master harness did not read ${harness} after saving`);
}

export const checks = [{
  id: "N-D26", op: ["OP-28"], title: "Switching the master harness keeps one conversation and one message box",
  why: "owner: \"Switching the master harness shows another conversation and two input boxes\"",
  after: "end",
  run: async (kit) => {
    await kit.tab("conversation");
    await startIfPaused(kit, kit.project());
    const messages = messageCount(await liveLine());
    if (!messages) throw new Error(`the trial session should be live with messages; the Sessions bar reads "${await liveLine()}"`);
    const seen = [];
    for (const { id, name } of SWITCHES) {
      await chooseMaster(kit, id);
      await kit.tab("conversation");
      await startIfPaused(kit, kit.project());
      let verdict = null;
      await kit.waitFor(async () => {
        verdict = switchSeen(await area(), name, messages);
        return { done: verdict.done, seen: verdict.seen };
      }, REPLY_MS, `the master area did not settle on ${name}`);
      await kit.screen();
      if (!verdict.ok) throw new Error(`after switching to ${name}: ${verdict.seen}`);
      seen.push(`${name}: ${verdict.seen}`);
    }
    return seen.join("; ");
  },
}];
