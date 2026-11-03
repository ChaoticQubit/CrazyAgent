import { cardOf, readCards, roleName } from "../agents-cards.mjs";

export const checks = [{
  id: "N-D23",
  op: ["OP-32"],
  title: "Every agent card names a persona and its @role, not Claude Code 1, 2, 3",
  why: "owner: \"what is cloud code 1, cloud code 2, cloud code 3? Can't we give proper names to these sub agents?\"",
  after: "team-tree",
  run: async (kit) => {
    await kit.tab("agents");
    const { nodes } = await kit.graph();
    const cards = await readCards();
    const agents = nodes.filter((node) => node.kind !== "master" && node.agent != null);
    const panes = nodes.filter((node) => node.kind === "teammate" && node.agent == null).map((node) => node.label);
    if (agents.length < 2) throw new Error(`the graph holds ${agents.length} agents with a team row; the trial team has a master, two leads and their workers`);
    const wrong = agents.filter((node) => !roleName(node.label)).map((node) => node.label);
    if (wrong.length) throw new Error(`agents not named persona (@role): ${wrong.join(", ")}`);
    const undrawn = agents.filter((node) => !cardOf(cards, node)).map((node) => node.label);
    if (undrawn.length) throw new Error(`agents with no card under their name: ${undrawn.join(", ")}`);
    await kit.screen();
    return `${agents.length} agents named persona (@role) on their cards: ${agents.map((node) => node.label).join(", ")}${panes.length ? `; pane-only teammates, named by their pane and not judged: ${panes.join(", ")}` : ""}`;
  },
}];
