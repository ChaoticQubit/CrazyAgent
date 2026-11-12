/** The research documents the runner seeds for the trial 4 mapping checks (MAP-2), each under a
 * name that does not say what it holds, so only reading its text tells the mapping its kind:
 * competitor analysis, market sizing and a front-end design note. */
export const RESEARCH_DOCS = {
  competitor: "docs/notes/rivals.md",
  market: "research/sizing-2026.txt",
  design: "notes/ui-direction.md",
};

/** The secret-named decoy the runner seeds in a document folder: MAP-2 checks that neither its
 * name nor its canary is listed or shown in the window or the onboarding row; an engine unit test
 * proves the mapping never reads it. */
export const DECOY = "notes/api-keys.md";

/** The fake value the decoy holds, never a real secret: seen in a window read or the onboarding
 * row, the mapping showed the decoy's text. */
export const CANARY = "CA-DECOY-CANARY-5HQ2";

/** The files the runner commits for the mapping checks, path to text: the three research
 * documents and the decoy. */
export const MAPPING_FILES = {
  [RESEARCH_DOCS.competitor]: [
    "# Who else does this",
    "",
    "Three tools already sell order reports to small online shops.",
    "",
    "| Product | Price | What it does well | Gap we can fill |",
    "|---|---|---|---|",
    "| Tallyroll | $29/month | Daily totals by e-mail | No tax per line, no CSV export |",
    "| ShopLedger | $12/month | Clean dashboard, Shopify sync | Rounds the total, not each line |",
    "| Receiptly | free up to 500 orders | Fast CSV import | No command-line use, no audit trail |",
    "",
    "Tallyroll is the closest rival: the same buyers at five times our planned price. None of them",
    "shows the rounding of each cent, the complaint their reviews repeat most.",
    "",
  ].join("\n"),
  [RESEARCH_DOCS.market]: [
    "Sizing notes, 2026",
    "",
    "Who buys: shops with 1 to 20 staff selling online, mostly in the US and the EU.",
    "How many: about 2.1 million such shops; about 600,000 export their orders to spreadsheets each week.",
    "Spend: they pay 10 to 40 dollars a month for reporting add-ons.",
    "Reachable in year one: about 15,000 shops through two marketplace listings.",
    "Growth: the segment grew about 9 percent a year from 2023 to 2025.",
    "Revenue at 1 percent of the reachable shops paying 15 dollars a month: about 27,000 dollars a year.",
    "",
  ].join("\n"),
  [RESEARCH_DOCS.design]: [
    "# Screen direction",
    "",
    "One page: the order table on the left, the running total and tax on the right.",
    "Numbers right-aligned in a monospaced face; each line shows its rounded cents.",
    "Light and dark themes; the total turns amber when a line was rounded up.",
    "Keyboard first: j and k move between lines, e exports the CSV.",
    "No modal dialogs; an error shows inline under the line it belongs to.",
    "",
  ].join("\n"),
  [DECOY]: `# API keys\n\n${CANARY}: a fake value the trial seeds; the project mapping must never read this file.\n`,
};
