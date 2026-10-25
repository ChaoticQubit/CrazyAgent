import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const PLAIN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const IDENTIFIER = "(?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*)";
const SEMVER = new RegExp(`^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)(?:-${IDENTIFIER}(?:\\.${IDENTIFIER})*)?$`);
const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));

function fail(message) {
  throw new Error(message);
}

function escape(text) {
  return text.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
}

/**
 * Computes the release version for a channel. Input: `channel` "dev" with the plain workspace
 * `base` version and the build time `now`, or "stable" with the pushed `tag` (vX.Y.Z).
 * Output: `<base>-dev.<UTC yyyymmddHHMM>` for dev, `X.Y.Z` for stable.
 */
export function channelVersion({ channel, base, tag, now }) {
  if (channel === "stable") {
    const match = typeof tag === "string" ? /^(?:refs\/tags\/)?v(\d+\.\d+\.\d+)$/.exec(tag) : null;
    if (!match || !PLAIN.test(match[1])) fail("release_tag_rejected");
    return match[1];
  }
  if (channel !== "dev") fail("release_channel_rejected");
  if (typeof base !== "string" || !PLAIN.test(base)) fail("release_base_version_rejected");
  assert(now instanceof Date && Number.isFinite(now.getTime()), "release_clock_invariant");
  const pad = (value) => String(value).padStart(2, "0");
  const stamp = `${now.getUTCFullYear()}${pad(now.getUTCMonth() + 1)}${pad(now.getUTCDate())}${pad(now.getUTCHours())}${pad(now.getUTCMinutes())}`;
  const version = `${base}-dev.${stamp}`;
  assert(SEMVER.test(version), "release_dev_version_invariant");
  return version;
}

/**
 * Reads the `[workspace.package]` version from the workspace Cargo.toml under `root`.
 * Output: the version string.
 */
export async function workspaceVersion(root = REPO) {
  const text = await readFile(join(root, "Cargo.toml"), "utf8");
  const matches = [...text.matchAll(workspacePattern())];
  if (matches.length !== 1) fail("release_workspace_version_missing");
  return matches[0][2];
}

function workspacePattern() {
  return /^(\[workspace\.package\]\n(?:(?!\[)[^\n]*\n)*?version = ")([^"\n]*)(")/gm;
}

async function members(root) {
  const text = await readFile(join(root, "Cargo.toml"), "utf8");
  const list = /^members = \[([^\]]*)\]$/m.exec(text);
  if (!list) fail("release_workspace_members_missing");
  const paths = [...list[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
  if (paths.length === 0) fail("release_workspace_members_missing");
  return Promise.all(paths.map(async (path) => {
    const manifest = await readFile(join(root, path, "Cargo.toml"), "utf8");
    const name = /^\[package\]\nname = "([^"]+)"$/m.exec(manifest);
    if (!name || !/^version\.workspace = true$/m.test(manifest)) fail(`release_member_version_not_inherited:${path}`);
    return name[1];
  }));
}

function replaceOnce(text, pattern, version, label) {
  const matches = [...text.matchAll(pattern)];
  if (matches.length !== 1) fail(`release_version_anchor_rejected:${label}`);
  const [whole, head, , tail] = matches[0];
  return text.slice(0, matches[0].index) + head + version + tail + text.slice(matches[0].index + whole.length);
}

/**
 * Writes `version` into the workspace Cargo.toml, the workspace members' Cargo.lock entries,
 * apps/desktop/package.json, its package-lock.json and tauri.conf.json under `root`.
 * Every file is rewritten only after every edit was located once. Output: changed paths.
 */
export async function applyVersion(root, version) {
  if (typeof version !== "string" || !SEMVER.test(version)) fail("release_version_rejected");
  const names = await members(root);
  const edits = {
    "Cargo.toml": [[workspacePattern(), "workspace.package"]],
    "Cargo.lock": names.map((name) => [new RegExp(`^(\\[\\[package\\]\\]\\nname = "${escape(name)}"\\nversion = ")([^"\\n]*)("\\n(?!source = ))`, "gm"), name]),
    "apps/desktop/package.json": [[/^( {2}"version": ")([^"\n]*)(",?)$/gm, "version"]],
    "apps/desktop/package-lock.json": [
      [/^( {2}"version": ")([^"\n]*)(",?)$/gm, "version"],
      [/^( {4}"": \{\n(?: {6}[^\n]*\n)*? {6}"version": ")([^"\n]*)(")/gm, "packages.root.version"],
    ],
    "apps/desktop/src-tauri/tauri.conf.json": [[/^( {2}"version": ")([^"\n]*)(",?)$/gm, "version"]],
  };
  const written = [];
  for (const [file, patterns] of Object.entries(edits)) {
    let text = await readFile(join(root, file), "utf8");
    for (const [pattern, label] of patterns) text = replaceOnce(text, pattern, version, `${file}:${label}`);
    written.push([file, text]);
  }
  assert.equal(written.length, Object.keys(edits).length, "release_version_files_invariant");
  for (const [file, text] of written) await writeFile(join(root, file), text);
  return written.map(([file]) => file);
}

async function main(args) {
  const [command, value, ...rest] = args;
  if (rest.length !== 0) fail("release_arguments_rejected");
  if (command === "dev" && value === undefined) return channelVersion({ channel: "dev", base: await workspaceVersion(), now: new Date() });
  if (command === "stable" && value !== undefined) return channelVersion({ channel: "stable", tag: value });
  if (command === "current" && value === undefined) return workspaceVersion();
  if (command === "set" && value !== undefined) {
    await applyVersion(REPO, value);
    return value;
  }
  fail("usage: version.mjs dev | stable <tag> | current | set <version>");
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    console.log(await main(process.argv.slice(2)));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
