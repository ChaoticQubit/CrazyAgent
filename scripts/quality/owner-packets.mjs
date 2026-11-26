import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** The one file that may lay out a 37-byte owner packet, as `owner_packet` and its buffer. */
export const HELPER = "crates/crazyagent/src/engine/secret_frame.rs";

/** The tracked Rust files the check reads: the crate's sources and its integration tests. */
export const SCOPE = /^crates\/crazyagent\/(src|tests)\/.+\.rs$/;

/**
 * A zeroed 37-byte array, the shape of a hand-built owner packet: `0`, `0u8` or `0_u8`, by
 * the literal length or by the helper's `OWNER_PACKET_BYTES`, with or without a path.
 */
export const PACKET = /\[\s*0(?:_?u8)?\s*;\s*(?:37|(?:\w+::)*OWNER_PACKET_BYTES)\s*\]/g;

/** The largest `git ls-files` listing read, far above the repository's size. */
const LISTING_BYTES_MAX = 64 * 1024 * 1024;

/**
 * Finds the hand-built owner packets in one file's text: each 37-byte array literal.
 * Input: the text. Output: the 1-based line numbers of the literals, in order.
 */
export function packetLines(text) {
  const lines = [];
  for (const match of text.matchAll(PACKET)) {
    lines.push(text.slice(0, match.index).split("\n").length);
  }
  return lines;
}

/**
 * Checks the files' texts: a packet literal anywhere but the helper file is a problem.
 * Input: a map of repository path to text. Output: one problem line per literal.
 */
export function check(files) {
  const problems = [];
  for (const [file, text] of files) {
    if (file === HELPER || !SCOPE.test(file)) {
      continue;
    }
    for (const line of packetLines(text)) {
      problems.push(`${file}:${line}: build the owner packet with secret_frame::owner_packet`);
    }
  }
  return problems;
}

/**
 * Reads every tracked file in scope under `root` and checks them.
 * Input: the repository root. Output: the problem lines.
 */
export function run(root) {
  const listed = execFileSync("git", ["ls-files", "-z"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: LISTING_BYTES_MAX,
  })
    .split("\0")
    .filter((file) => SCOPE.test(file));
  const files = new Map(listed.map((file) => [file, readFileSync(path.join(root, file), "utf8")]));
  return check(files);
}

/** Prints the problems and exits 1 when there are any, for `make owner-packets-check`. */
function main() {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const root = path.resolve(process.argv[2] ?? path.join(here, "..", ".."));
  const problems = run(root);
  if (problems.length > 0) {
    console.error(problems.join("\n"));
    console.error(`${problems.length} hand-built owner packet(s); use ${HELPER}`);
    process.exit(1);
  }
  console.log("every owner packet is built by the helper");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
