import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const NOTES_MAX = 100_000;

const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-dev\.\d{12})?$/;
const CUT_NOTE = "\n\nThe rest of this version's notes is in CHANGELOG.md.";

/**
 * The stable version a release version belongs to: a dev build "<base>-dev.<stamp>" reads its base.
 * Input: a plain X.Y.Z or an X.Y.Z-dev.<12 digit stamp> version. Output: the X.Y.Z base.
 * Throws release_notes_version_rejected for any other shape.
 */
export function baseVersion(version) {
  const match = typeof version === "string" ? VERSION.exec(version) : null;
  if (match === null) throw new Error("release_notes_version_rejected");
  return `${match[1]}.${match[2]}.${match[3]}`;
}

/**
 * The CHANGELOG.md section of a release version, for its release notes.
 * Inputs: the changelog text and a stable or dev build version. Output: the lines under the
 * "## [<base>]" heading up to the next "## " heading, trimmed of blank edge lines and cut at a line
 * to NOTES_MAX characters, or "" when the changelog has no such section or it is empty.
 */
export function changelogSection(text, version) {
  const heading = `## [${baseVersion(version)}]`;
  const lines = text.split("\n");
  const start = lines.findIndex((line) => line === heading || line.startsWith(`${heading} `));
  if (start < 0) return "";
  const after = lines.slice(start + 1);
  const end = after.findIndex((line) => line.startsWith("## "));
  const section = (end < 0 ? after : after.slice(0, end)).join("\n").replace(/^\s*\n/, "").trimEnd();
  const room = NOTES_MAX - CUT_NOTE.length;
  const line = section.lastIndexOf("\n", room);
  const kept = section.length <= NOTES_MAX ? section : `${section.slice(0, line > 0 ? line : room)}${CUT_NOTE}`;
  assert(kept.length <= NOTES_MAX, "release notes stay within their bound");
  assert(!/^## /m.test(kept), "a section never holds another version's heading");
  return kept;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const [file, version] = process.argv.slice(2);
    baseVersion(version);
    const text = existsSync(file ?? "") ? readFileSync(file, "utf8") : "";
    const section = changelogSection(text, version);
    if (section !== "") process.stdout.write(`${section}\n`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
