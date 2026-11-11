import { UI_MS, notShown, overlapping, readCards, spilled, summary } from "../agents-cards.mjs";

export const checks = [{
  id: "N-D17",
  op: ["OP-30", "OP-31"],
  title: "After Changes and back, the graph draws every node with no overlap and its text inside the cards",
  why: "owner: \"I switched to a tab different tab I switched back and now the graph is not generating\"; \"there's so much text everywhere and it gets overlapped\"",
  after: "team-tree",
  run: async (kit) => {
    await kit.tab("agents");
    await kit.until(async () => (await readCards()).length > 0, UI_MS, "the Agents tab drew no card before the switch");
    await kit.tab("changes");
    await kit.tab("agents");
    const drawn = await kit.waitFor(async () => {
      const { nodes } = await kit.graph();
      const cards = await readCards();
      const left = notShown(await summary());
      const want = nodes.length - left;
      return { done: want > 0 && cards.length === want, seen: `${cards.length} cards for ${nodes.length} nodes, ${left} not shown` };
    }, UI_MS, "the graph did not draw every node after switching to Changes and back");
    const cards = await readCards();
    const pairs = overlapping(cards);
    if (pairs.length) throw new Error(`overlapping cards: ${pairs.map(([a, b]) => `${a} / ${b}`).join("; ")}`);
    const out = spilled(cards);
    if (out.length) throw new Error(`text outside its card: ${out.map((item) => `${item.card}: ${JSON.stringify(item.text)}`).join("; ")}`);
    await kit.screen();
    return `after Changes and back: ${drawn}; no two of ${cards.length} cards overlap and every card's text stays inside it`;
  },
}];
