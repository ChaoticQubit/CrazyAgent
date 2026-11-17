import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

export const WORKFLOWS = new URL("../../.github/workflows/", import.meta.url);

export async function workflow(name) {
  const text = await readFile(new URL(name, WORKFLOWS), "utf8");
  assert(!text.includes("\r"), `${name} has LF line endings`);
  return text;
}

/**
 * Returns the lines of one top-level section (`on:`, `jobs:`) or of one job under `jobs:`.
 * Inputs: the workflow text, the indentation of the key and its name. Output: the key's line
 * and every following line indented deeper, so a test can read one block without a YAML parser.
 */
export function block(text, indent, name) {
  const lines = text.split("\n");
  const start = lines.indexOf(`${" ".repeat(indent)}${name}:`);
  assert(start >= 0, `no ${name} block at indent ${indent}`);
  const end = lines.findIndex((line, index) => index > start && line.trim() !== "" && line.length - line.trimStart().length <= indent);
  return lines.slice(start, end < 0 ? lines.length : end);
}

/**
 * Lists the jobs under `jobs:` of a workflow, failing closed: every non-blank line of the block
 * deeper than 2-space indent belongs to a job whose key sits alone at 2-space indent, `#`-led
 * run block lines included, since GitHub expands them; only a `#` line at indent 2 or less is a
 * YAML comment passed over. A job the guards cannot read throws instead of being skipped.
 * Input: the workflow text. Output: each job's name with its lines, at least one.
 */
export function jobsOf(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("jobs:");
  assert(start >= 0, "no jobs: block at indent 0, so no job can be read");
  const jobs = [];
  for (const line of lines.slice(start + 1)) {
    if (line.trim() === "") continue;
    if (line.trimStart().startsWith("#") && line.length - line.trimStart().length <= 2) continue;
    if (/^\S/.test(line)) break;
    const name = line.match(/^ {2}([A-Za-z0-9_-]+):\s*$/)?.[1];
    if (name) {
      jobs.push({ name, lines: [line] });
      continue;
    }
    assert(jobs.length > 0 && line.length - line.trimStart().length > 2, `every job key sits alone at 2-space indent under jobs:, not ${JSON.stringify(line)}`);
    jobs.at(-1).lines.push(line);
  }
  assert(jobs.length > 0, "no job key at 2-space indent under jobs:");
  return jobs;
}

/**
 * The lines of a workflow outside its `jobs:` block, such as a workflow-level `env:` every job
 * inherits; the block ends where jobsOf stops reading. Input: the workflow text. Output: the lines.
 */
function sharedLines(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("jobs:");
  const end = lines.findIndex((line, index) => index > start && /^[^\s#]/.test(line));
  return [...lines.slice(0, start), ...(end < 0 ? [] : lines.slice(end))];
}

/**
 * Matches workflow text that reads a secret, in any form GitHub expands and in any case, as the
 * runner names contexts without case: `secrets.NAME`, `SECRETS['NAME']`, a whole-context call
 * such as `toJSON(secrets)`, or the bare context.
 */
export const SECRET = /secrets\.|\bsecrets\b/i;

/**
 * Whether a YAML line leaves a quoted scalar open, so it runs on into the next lines: a quote
 * opens a scalar only where a node starts, after the indent, `- `, `: `, `? `, `[`, `{` or `,`
 * and any anchor (`&a`) or tag (`!!str`) in front of it, and a `#` after a space outside one
 * starts a comment. Input: one line outside a block scalar. Output: the answer.
 */
function leftOpen(line) {
  let quote = "";
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (quote !== "") {
      if (char !== quote) continue;
      if (quote === "'" && line[index + 1] === "'") index += 1;
      else quote = "";
    } else if (char === "#" && /(?:^|\s)$/.test(line.slice(0, index))) {
      return false;
    } else if ((char === "'" || char === '"') && /(?:^\s*|[-:?]\s+|[[{,]\s*)(?:[&!]\S*\s+)*$/.test(line.slice(0, index))) {
      quote = char;
    }
  }
  return quote !== "";
}

/**
 * Lists the lines of a workflow, outside block scalars (`|`, `>`), that YAML reads as more than
 * their text: an alias, which hands a job another's keys without their words; a backslash, which
 * a double-quoted scalar decodes; and a quoted scalar left open, which runs on into lines a block
 * indicator would hide (B17 F1). Input: the workflow text. Output: the lines.
 */
function unreadLines(text) {
  const unread = [];
  let block = -1;
  for (const line of text.split("\n")) {
    const indent = line.length - line.trimStart().length;
    if (block >= 0 && (line.trim() === "" || indent > block)) continue;
    const opener = line.match(/^(\s*(?:- +)*)(?:[^\s#'"][^#'"]*:\s+)?[|>][-+0-9]*\s*(?:#.*)?$/);
    block = opener ? opener[1].length : -1;
    if (line.includes("\\") || /(?:^\s*|[-:?]\s+|[[{,]\s*)\*\S/.test(line) || leftOpen(line)) unread.push(line);
  }
  return unread;
}

/**
 * Names the jobs of a workflow that read a secret, hand secrets to a called workflow or bind an
 * environment, whatever the file or job; a secret outside `jobs:` reaches every job. A workflow
 * whose YAML says more than its lines ([unreadLines]) is refused, as is one whose jobs do not
 * parse. Input: the workflow text. Output: the job names in file order.
 */
export function exposedJobs(text) {
  const jobs = jobsOf(text);
  const unread = unreadLines(text);
  assert(unread.length === 0, `the secret guard reads YAML line by line, so it refuses an alias, a backslash or a quoted scalar left open outside a block scalar: ${JSON.stringify(unread)}`);
  const shared = sharedLines(text).some((line) => SECRET.test(line));
  return jobs.filter(({ lines }) => shared || lines.some((line) => SECRET.test(line) || /^ {4}(environment|secrets):/.test(line))).map(({ name }) => name);
}

/**
 * Lists the lines of a local composite action the secret guard refuses, by the workflow rules:
 * a line YAML reads as more than its text ([unreadLines]), and a line that reads a secret or
 * binds an environment, since every job that uses the action runs it. Input: the action text.
 * Output: the lines.
 */
export function actionSecretLines(text) {
  return [...unreadLines(text), ...text.split("\n").filter((line) => SECRET.test(line) || /^\s*(environment|secrets):/.test(line))];
}
