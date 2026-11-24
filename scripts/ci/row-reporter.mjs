import { relative } from "node:path";

/** The longest failure reason a row line carries, so one long error cannot flood the log. */
const REASON_MAX = 300;

/** One result line for a test row: `ROW <file> :: <name> :: <ok|not ok|skip>`, with the first
 * line of the failure after it for a failed row. Inputs: the file (relative to the working
 * folder), the row's name, its result and the failure reason. Output: the line. */
export function rowLine(file, name, result, reason = "") {
  const flat = (text) => String(text).replace(/\s+/g, " ").trim();
  const told = flat(reason).slice(0, REASON_MAX);
  return `ROW ${flat(file)} :: ${flat(name)} :: ${result}${told ? ` :: ${told}` : ""}\n`;
}

/** The tally line after every row. Input: the counts by result. Output: the line. */
export function tallyLine(counts) {
  return `ROWS ok ${counts.ok} not-ok ${counts["not ok"]} skip ${counts.skip}\n`;
}

/** A node:test reporter that prints one [`rowLine`] for each test that is not a suite, as it
 * ends, and [`tallyLine`] last, so a log that cannot be summarized still names every row and
 * its result. Input: the test events. Output: the lines. */
export default async function* rowReporter(source) {
  const counts = { ok: 0, "not ok": 0, skip: 0 };
  for await (const { type, data } of source) {
    if (type !== "test:pass" && type !== "test:fail") continue;
    if (data.details?.type === "suite") continue;
    const result = data.skip || data.todo ? "skip" : type === "test:pass" ? "ok" : "not ok";
    counts[result] += 1;
    const file = data.file ? relative(process.cwd(), data.file) : "(no file)";
    const reason = result === "not ok" ? String(data.details?.error?.cause?.message ?? data.details?.error?.message ?? "").split("\n")[0]
      : typeof data.skip === "string" ? data.skip : "";
    yield rowLine(file, data.name, result, reason);
  }
  yield tallyLine(counts);
}
