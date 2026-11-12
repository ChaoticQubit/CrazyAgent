import { texts } from "../settings-steps.mjs";

/** The badge words the Questions panel shows for each question severity. */
export const SEVERITY_WORDS = ["Urgent", "Waiting", "Carrying on"];

/** Reads the tags of one Questions panel row: the severity badge it opens with and who it is
 * for, as "For: Me" or "For: <agent>". Input: the row's text. Output: { severity, forWhom },
 * each null when the row does not show it. */
export function questionTags(row) {
  const severity = SEVERITY_WORDS.find((word) => row.trimStart().startsWith(word)) ?? null;
  const found = /For:\s*(Me|@?[\w-]+(?: \(@[\w-]+\))?)/.exec(row);
  return { severity, forWhom: found ? found[1] : null };
}

const ITEMS = "ul[aria-label=\"Open questions\"] li.tg-question";

export const checks = [{
  id: "N-D29", op: ["OP-50"], title: "A question in the Questions panel shows who it is for and its severity",
  why: "owner: \"Agent questions carry no for whom and severity tags\"",
  after: "team-tree",
  run: async (kit) => {
    await kit.tab("agents");
    await kit.waitFor(async () => {
      const count = (await texts(ITEMS)).length;
      return { done: count > 0, seen: `${count} open questions` };
    }, kit.team(), "no agent question reached the Questions panel");
    const rows = await texts(ITEMS);
    await kit.screen();
    const tags = rows.map(questionTags);
    const missing = tags.flatMap((tag, at) => [
      ...(tag.severity ? [] : [`question ${at + 1} shows no severity`]),
      ...(tag.forWhom ? [] : [`question ${at + 1} shows no For: tag`]),
    ]);
    if (missing.length > 0) throw new Error(`${missing.join("; ")} (${rows.length} open)`);
    return tags.map((tag, at) => `question ${at + 1}: ${tag.severity}, For: ${tag.forWhom}`).join("; ");
  },
}];
