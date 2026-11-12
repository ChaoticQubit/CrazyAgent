import { UI_MS, cardOf, readCards, showsTask, shownView, statusWord } from "../agents-cards.mjs";

const NOT_RUNNING = "isn't running";

/** Reads the working leads and sub-agents with the card each is drawn as. Input: the kit.
 * Output: the rows, each { node, card }, card undefined when the node is not drawn. */
async function working(kit) {
  const { nodes } = await kit.graph();
  const cards = await readCards();
  return nodes.filter((node) => (node.kind === "lead" || node.kind === "sub") && node.status === "working")
    .map((node) => ({ node, card: cardOf(cards, node) }));
}

/** What is wrong with one drawn working card: it does not name its status or shows no task
 * line. Input: the row. Output: the problems, empty when the card is right. */
function problems({ node, card }) {
  const word = statusWord(node);
  return [
    ...(word && card.label.includes(word) ? [] : [`${node.label} does not say ${word || "its status"}`]),
    ...(showsTask(card) ? [] : [`${node.label} shows no task line (${JSON.stringify(card.task[0] ?? "")})`]),
  ];
}

export const checks = [{
  id: "N-D7",
  op: ["OP-16", "OP-29"],
  title: "Each working agent's card says Working, shows its task and opens its terminal",
  why: "owner: \"it never shows what the sub agent is doing\"",
  after: "team-tree",
  run: async (kit) => {
    await kit.tab("agents");
    await kit.waitFor(async () => {
      const rows = await working(kit);
      return { done: rows.some((row) => row.card), seen: rows.map((row) => `${row.node.label}${row.card ? "" : " (not drawn)"}`).join("; ") || "no working lead or sub-agent" };
    }, kit.team(), "no working lead or sub-agent card was drawn");
    let drawn = [];
    await kit.waitFor(async () => {
      drawn = (await working(kit)).filter((row) => row.card);
      const wrong = drawn.flatMap(problems);
      return { done: drawn.length > 0 && wrong.length === 0, seen: wrong.join("; ") || "no working card drawn" };
    }, UI_MS, "a working card did not name its status and task");
    const opener = drawn.find((row) => row.card.opens);
    if (!opener) throw new Error(`no working card opens a terminal: ${drawn.map((row) => row.node.label).join(", ")}`);
    const label = opener.node.label;
    const view = await shownView(kit, label);
    if (view.state.includes(NOT_RUNNING)) throw new Error(`${label}'s terminal says: ${view.state}`);
    if (!view.rows.trim()) throw new Error(`${label}'s terminal shows no rows; it says: ${view.state}`);
    await kit.screen();
    return `${drawn.length} working cards say ${[...new Set(drawn.map((row) => statusWord(row.node)))].join("/")} and show a task line (${drawn.map((row) => row.node.label).join(", ")}); ${label}'s card opened its terminal${view.live ? ", live" : ""}`;
  },
}];
