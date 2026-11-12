import { click } from "../settings-steps.mjs";
import { RUNNING, UI_MS, backTo, cardOf, finishedList, finishedRow, openFinished, readCards } from "../agents-cards.mjs";

const IDLE_WAIT_MS = 20 * 60 * 1000;
const WAKE_MS = 5 * 60 * 1000;
const OWNER_STOP = "Stopped by you";
const FINISHED_COUNT = /^Finished \((\d+)\)$/;

/** Waits under Show for a sub-agent or teammate the engine finished on its own, listed in the
 * Finished fold-out and not stopped by the owner. The fold-out is on the Agents tab, so each
 * read first takes a window that left it back there and names the page it was on. Input: the
 * kit. Output: { label, agent, moves }, moves the pages the window was taken back from. */
async function leftAlone(kit) {
  let found = null;
  const moves = [];
  await kit.waitFor(async () => {
    await backToAgents(kit, moves);
    const { nodes } = await kit.graph();
    const { rows } = await finishedList();
    const ended = nodes.filter((node) => (node.kind === "sub" || node.kind === "teammate") && node.agent != null && (node.status === "stopped" || node.status === "failed"));
    const match = ended.find((node) => {
      const row = finishedRow(rows, node.label);
      return row && !row.ended.startsWith(OWNER_STOP);
    });
    found = match ? { label: match.label, agent: match.agent, moves } : null;
    return { done: found !== null, seen: `${ended.length} ended agents (${ended.map((node) => node.label).join(", ") || "none"}); Finished list: ${rows.map((row) => `${row.name} ${row.ended}`).join("; ") || "empty"}${awayNote(moves)}` };
  }, Math.min(kit.team(), IDLE_WAIT_MS), "no sub-agent left on its own after its idle stop");
  return found;
}

/** Takes the window back to the Agents tab when it shows another page and notes that page,
 * as each Finished fold-out read does first. Inputs: the kit and the pages noted so far.
 * Output: none; a page the window was taken back from is added to the pages. */
async function backToAgents(kit, moves) {
  const away = await backTo(kit, "agents");
  if (away !== null) moves.push(away);
}

/** Names the pages the window was taken back to Agents from, for a check's detail. Input: the
 * hashes. Output: the clause, empty when the window never left. */
function awayNote(moves) {
  return moves.length === 0 ? "" : `; the window left Agents ${moves.length} times (${[...new Set(moves)].join(", ")}) and was taken back`;
}

/** Hides finished agents and checks the agent left the graph for the Finished fold-out,
 * noting each page the window was taken back from. Inputs: the kit and the agent, whose
 * moves gain those pages. Output: the fold-out's toggle text. */
async function hidden(kit, { label, agent, moves }) {
  await kit.choose("agents", "Finished agents", "Hide");
  let toggle = "";
  await kit.until(async () => {
    await backToAgents(kit, moves);
    const { nodes } = await kit.graph();
    const cards = await readCards();
    const list = await finishedList();
    toggle = list.label;
    const count = FINISHED_COUNT.exec(toggle);
    return !nodes.some((node) => node.agent === agent) && !cardOf(cards, { label }) && Boolean(count) && Number(count[1]) >= 1 && Boolean(finishedRow(list.rows, label));
  }, UI_MS, `under Hide, ${label} did not leave the graph for the Finished list`).catch((error) => {
    throw new Error(`${error.message}${awayNote(moves)}`);
  });
  return toggle;
}

/** Wakes the agent from the Finished fold-out and waits until the same agent row runs again,
 * with no second agent under its name, noting the page the window was taken back from.
 * Inputs: the kit and the agent, whose moves gain that page. Output: its status. */
async function woken(kit, { label, agent, moves }) {
  await backToAgents(kit, moves);
  const row = finishedRow((await finishedList()).rows, label);
  if (!row) throw new Error(`${label} is not in the Finished list to wake${awayNote(moves)}`);
  if (!row.wakeable) throw new Error(`${label}'s Wake is disabled${row.note ? `: ${row.note}` : ""}${awayNote(moves)}`);
  await openFinished(kit);
  await click(`//details[@aria-label="Finished agents"]//ul[@aria-label="Finished agents"]/li[.//span[contains(concat(" ", normalize-space(@class), " "), " pj-kind ")][normalize-space(text())="${label}"]]//button[normalize-space(.)="Wake"]`);
  let status = "";
  await kit.waitFor(async () => {
    const { nodes } = await kit.graph();
    const same = nodes.filter((node) => node.label === label);
    status = same.find((node) => node.agent === agent)?.status ?? "";
    const others = same.filter((node) => node.agent !== agent);
    return { done: RUNNING.includes(status) && others.length === 0, seen: `agent ${agent} ${status || "not in the graph"}; other agents named ${label}: ${others.length}${awayNote(moves)}` };
  }, Math.min(kit.team(), WAKE_MS), `${label} did not run again as the same agent after Wake`);
  return status;
}

export const checks = [{
  id: "N-D25",
  op: ["OP-27"],
  title: "A sub-agent that finished leaves the graph under Hide and Wake brings the same agent back",
  why: "owner: \"Why are all the sub-agents that have done the work, that are idle for so long, why are they not leaving?\"",
  after: "changes",
  run: async (kit) => {
    await kit.tab("agents");
    const agent = await leftAlone(kit);
    let failure = null;
    let detail = "";
    try {
      const toggle = await hidden(kit, agent);
      await kit.screen();
      const status = await woken(kit, agent);
      await kit.screen();
      detail = `${agent.label} left on its own; under Hide it left the graph for ${toggle}; Wake ran the same agent ${agent.agent} again (${status})${awayNote(agent.moves)}`;
    } catch (error) {
      failure = error;
    }
    const restored = await kit.choose("agents", "Finished agents", "Show").then(() => null, (error) => error);
    if (failure) throw restored ? new Error(`${failure.message}; Finished agents could not be set back to Show: ${restored.message}`) : failure;
    if (restored) throw new Error(`Finished agents could not be set back to Show for the trial's final tree: ${restored.message}`);
    return `${detail}; Finished agents set back to Show`;
  },
}];
