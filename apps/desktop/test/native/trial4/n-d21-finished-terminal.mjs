import { RUNNING, cardOf, readCards, shownView } from "../agents-cards.mjs";

const NOT_RUNNING = "isn't running";

export const checks = [{
  id: "N-D21",
  op: ["OP-39"],
  title: "A finished sub-agent's card opens its terminal on its last screen",
  why: "owner: \"when I'm clicking on a sub-agent to open its terminal, it is giving me this error. This agent's terminal isn't running.\"",
  after: "final-tree",
  run: async (kit) => {
    await kit.tab("agents");
    const { nodes } = await kit.graph();
    const cards = await readCards();
    const ended = nodes.filter((node) => node.kind === "sub" && node.agent != null && !RUNNING.includes(node.status));
    if (!ended.length) throw new Error("no finished sub-agent in the graph; trial 3 sets Finished agents to Show, so each should be drawn");
    const opener = ended.find((node) => cardOf(cards, node)?.opens);
    if (!opener) throw new Error(`no finished sub-agent card opens a terminal: ${ended.map((node) => `${node.label} (${node.status}, terminal ${node.terminal ?? "none"})`).join("; ")}`);
    const view = await shownView(kit, opener.label);
    if (view.state.includes(NOT_RUNNING)) throw new Error(`${opener.label}'s terminal says: ${view.state}`);
    if (!view.rows.trim()) throw new Error(`${opener.label}'s terminal showed no last screen; it says: ${view.state}`);
    await kit.screen();
    const lines = view.rows.split("\n").filter((line) => line.trim()).length;
    return `${opener.label} (${opener.status}) opened on its last screen, ${lines} lines${view.state.trim() ? `; it says: ${view.state.trim().split("\n")[0]}` : ""}`;
  },
}];
