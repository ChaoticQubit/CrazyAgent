import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const PREFIX = "farm-junit-";
const FAILED_SHOWN = 300;
const ENTITIES = { lt: "<", gt: ">", amp: "&", quot: '"', apos: "'" };

/**
 * Decodes the five XML entities and numeric references in an attribute value; a numeric
 * reference outside Unicode stays as written. Input: the raw attribute text. Output: the plain text.
 */
function decode(text) {
  return text.replace(/&(lt|gt|amp|quot|apos|#\d+|#x[0-9a-fA-F]+);/g, (whole, name) => {
    if (!name.startsWith("#")) return ENTITIES[name];
    const code = name.startsWith("#x") ? parseInt(name.slice(2), 16) : Number(name.slice(1));
    return Number.isSafeInteger(code) && code <= 0x10ffff ? String.fromCodePoint(code) : whole;
  });
}

/**
 * Reads one attribute of an element's attribute text. Inputs: the attribute text and the name.
 * Output: the decoded value, or "" when absent.
 */
function attribute(attributes, name) {
  const match = attributes.match(new RegExp(`(?:^|\\s)${name}="([^"]*)"`));
  return match ? decode(match[1]) : "";
}

/**
 * Counts one JUnit file as nextest or node --test-reporter=junit writes it. Input: the XML text.
 * Output: { tests, failed (names of cases with a failure or error), skipped, seconds (the root
 * testsuites time, or null) }.
 */
export function count(xml) {
  const result = { tests: 0, failed: [], skipped: 0, seconds: null };
  for (const [, attributes, body = ""] of xml.matchAll(/<testcase\b((?:"[^"]*"|'[^']*'|[^>"'])*?)(?:\/>|>([\s\S]*?)<\/testcase>)/g)) {
    result.tests += 1;
    if (/<(?:failure|error)\b/.test(body)) result.failed.push(`${attribute(attributes, "classname")} ${attribute(attributes, "name")}`);
    else if (/<skipped\b/.test(body)) result.skipped += 1;
  }
  const time = xml.match(/<testsuites\b[^>]*?\stime="([0-9.]+)"/);
  if (time) result.seconds = Number(time[1]);
  return result;
}

/**
 * Builds the job summary for one farm run. Inputs: the expected part names, a map from part name
 * to its JUnit texts, the needed jobs' results ({ job: { result } }) and how many JUnit files
 * belong to no part. Output: { ok, text }; ok only when every expected part, and no other,
 * reported a test case, none failed, every job succeeded and no file is unattributed.
 */
export function summarize(expected, files, needs, unattributed = 0) {
  assert(needs !== null && typeof needs === "object" && !Array.isArray(needs), "needs is an object of job results");
  assert(Number.isSafeInteger(unattributed) && unattributed >= 0, "unattributed is a file count");
  const rows = ["| part | tests | failed | skipped | seconds |", "| - | - | - | - | - |"];
  const problems = [];
  if (unattributed > 0) problems.push(`unattributed JUnit: ${unattributed} file(s) outside farm-junit-<part> folders`);
  for (const [job, { result }] of Object.entries(needs)) {
    assert(typeof result === "string" && result.length > 0, `job ${job} has a result`);
    if (result !== "success") problems.push(`job ${job}: ${result}`);
  }
  const failed = [];
  const total = { tests: 0, failed: 0, skipped: 0 };
  for (const part of expected) {
    const texts = files.get(part) ?? [];
    if (texts.length === 0) {
      rows.push(`| ${part} | missing | - | - | - |`);
      problems.push(`missing: ${part}`);
      continue;
    }
    const counts = texts.map(count);
    const tests = counts.reduce((sum, one) => sum + one.tests, 0);
    const names = counts.flatMap((one) => one.failed);
    const skipped = counts.reduce((sum, one) => sum + one.skipped, 0);
    const timed = counts.filter((one) => one.seconds !== null);
    const seconds = timed.length === 0 ? "-" : String(Math.round(timed.reduce((sum, one) => sum + one.seconds, 0)));
    rows.push(`| ${part} | ${tests} | ${names.length} | ${skipped} | ${seconds} |`);
    if (tests === 0) problems.push(`no test case: ${part}`);
    failed.push(...names.map((name) => `- ${part} ${name}`));
    total.tests += tests;
    total.failed += names.length;
    total.skipped += skipped;
  }
  for (const part of files.keys()) if (!expected.includes(part)) problems.push(`unexpected: ${part}`);
  rows.push(`| all | ${total.tests} | ${total.failed} | ${total.skipped} | - |`);
  const shown = failed.slice(0, FAILED_SHOWN);
  if (failed.length > shown.length) shown.push(`- and ${failed.length - shown.length} more`);
  const text = ["## Test farm", "", ...rows, "", ...problems, ...(problems.length ? [""] : []), `### Failed (${failed.length})`, "", ...shown, ""].join("\n");
  return { ok: problems.length === 0 && failed.length === 0, text };
}

/**
 * Lists every .xml file under a folder, nested folders included. Input: the folder path.
 * Output: the file paths.
 */
function xmlFiles(folder) {
  return readdirSync(folder, { recursive: true }).filter((name) => name.endsWith(".xml")).map((name) => join(folder, name));
}

/**
 * Reads the downloaded JUnit files by part. download-artifact puts each artifact in its own
 * farm-junit-<part> folder, but a lone matched artifact straight into the root, with no folder
 * naming its part. So files outside every such folder count as the one expected part only when
 * FARM_PARTS names exactly one and no part folder exists; otherwise they are unattributed.
 * Inputs: the root and the expected parts. Output: { files (part -> texts), unattributed (count) }.
 */
export function partFiles(root, expected) {
  const files = new Map();
  const loose = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory() && entry.name.startsWith(PREFIX)) files.set(entry.name.slice(PREFIX.length), xmlFiles(path).map((file) => readFileSync(file, "utf8")));
    else if (entry.isDirectory()) loose.push(...xmlFiles(path));
    else if (entry.isFile() && entry.name.endsWith(".xml")) loose.push(path);
  }
  if (loose.length > 0 && files.size === 0 && expected.length === 1) {
    files.set(expected[0], loose.map((file) => readFileSync(file, "utf8")));
    return { files, unattributed: 0 };
  }
  return { files, unattributed: loose.length };
}

/**
 * Prints the summary of the JUnit files under argv[2] ([`partFiles`]) for the parts in FARM_PARTS
 * and the jobs in FARM_NEEDS (the workflow's toJSON(needs)), and sets exit status 1 unless the run
 * is ok. Inputs: argv and the environment. Output: stdout.
 */
function main(argv, env) {
  const root = argv[2];
  if (!root) throw new Error("usage: junit-summary.mjs <folder holding farm-junit-<part> folders>");
  if (!env.FARM_PARTS?.trim()) throw new Error("FARM_PARTS is not set");
  if (!env.FARM_NEEDS?.trim()) throw new Error("FARM_NEEDS is not set");
  const needs = JSON.parse(env.FARM_NEEDS);
  if (needs === null || typeof needs !== "object" || Array.isArray(needs)) throw new Error("FARM_NEEDS is not an object of job results");
  for (const [job, value] of Object.entries(needs)) if (typeof value?.result !== "string") throw new Error(`FARM_NEEDS has no result for ${job}`);
  const expected = env.FARM_PARTS.split(/\s+/).filter(Boolean);
  const { files, unattributed } = partFiles(root, expected);
  const { ok, text } = summarize(expected, files, needs, unattributed);
  process.stdout.write(text);
  process.exitCode = ok ? 0 : 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main(process.argv, process.env);
