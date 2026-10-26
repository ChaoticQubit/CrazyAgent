import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, readFile, readdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import { TARGETS, cliArchiveName, desktopLayout, targetInfo, verifySignature } from "./assets.mjs";

const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const STABLE = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const DEV = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-dev\.\d{12}$/;
const OUTPUTS = ["latest.json", "cli-latest.json", "SHA256SUMS"];

function fail(message) {
  throw new Error(message);
}

async function isFile(path) {
  try {
    return (await lstat(path)).isFile();
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

async function sha256File(path) {
  const hash = createHash("sha256");
  await pipeline(createReadStream(path), hash);
  return hash.digest("hex");
}

/**
 * Maps a target's desktop bundles to the Tauri updater platform keys that point at them:
 * `<os>-<arch>` plus the installer-specific `<os>-<arch>-<installer>` keys. Output: [key, asset] pairs.
 */
export function updaterPlatforms(target, version) {
  const os = { macos: "darwin", linux: "linux", windows: "windows" }[targetInfo(target).os];
  const arch = target.split("-")[0];
  const [first, second] = desktopLayout(target, version);
  if (os === "darwin") return [[`${os}-${arch}`, first.to], [`${os}-${arch}-app`, first.to]];
  if (os === "linux") return [[`${os}-${arch}`, first.to], [`${os}-${arch}-appimage`, first.to], [`${os}-${arch}-deb`, second.to]];
  return [[`${os}-${arch}`, first.to], [`${os}-${arch}-nsis`, first.to]];
}

/**
 * Checks that `dir` holds exactly the signed desktop bundles and CLI archives of `version` for
 * `targets`, verifies every signature against `publicKey` and the version, then writes
 * `latest.json` (Tauri updater format), `cli-latest.json` and `SHA256SUMS` over every file.
 * Output: the three written paths.
 */
export async function writeManifests({ dir, version, channel, commit, baseUrl, notes = "", pubDate, publicKey, targets = Object.keys(TARGETS) }) {
  if (channel !== "stable" && channel !== "dev") fail("release_channel_rejected");
  if (typeof version !== "string" || !(channel === "stable" ? STABLE : DEV).test(version)) fail("release_channel_version_rejected");
  if (typeof commit !== "string" || !/^[0-9a-f]{40}$/.test(commit)) fail("release_commit_rejected");
  if (typeof baseUrl !== "string" || !/^https:\/\/[^\s?#]+[^/\s?#]$/.test(baseUrl)) fail("release_base_url_rejected");
  if (typeof pubDate !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(pubDate) || Number.isNaN(Date.parse(pubDate))) fail("release_pub_date_rejected");
  if (typeof notes !== "string" || notes.length > 4096) fail("release_notes_rejected");
  if (!Array.isArray(targets) || targets.length === 0 || new Set(targets).size !== targets.length) fail("release_targets_rejected");
  for (const target of targets) targetInfo(target);
  const assets = [...targets.flatMap((target) => desktopLayout(target, version).map((item) => item.to)), ...targets.map((target) => cliArchiveName(version, target))];
  const expected = assets.flatMap((name) => [name, `${name}.sig`]);
  for (const name of expected) if (!(await isFile(join(dir, name)))) fail(`release_asset_missing:${name}`);
  for (const name of await readdir(dir)) {
    if (OUTPUTS.includes(name)) fail(`release_asset_exists:${name}`);
    if (!expected.includes(name)) fail(`release_asset_unexpected:${name}`);
  }
  const signatures = new Map();
  for (const name of assets) {
    const signature = (await readFile(join(dir, `${name}.sig`), "utf8")).trim();
    await verifySignature({ file: join(dir, name), signature, publicKey, version });
    signatures.set(name, signature);
  }
  const url = (name) => `${baseUrl}/${encodeURIComponent(name)}`;
  const platforms = {};
  for (const target of targets) for (const [key, name] of updaterPlatforms(target, version)) platforms[key] = { signature: signatures.get(name), url: url(name) };
  const cli = {};
  for (const target of targets) {
    const name = cliArchiveName(version, target);
    cli[target] = { url: url(name), sha256: await sha256File(join(dir, name)), signature: signatures.get(name) };
  }
  await writeFile(join(dir, "latest.json"), `${JSON.stringify({ version, notes, pub_date: pubDate, platforms }, null, 2)}\n`, { flag: "wx" });
  await writeFile(join(dir, "cli-latest.json"), `${JSON.stringify({ version, channel, commit, pub_date: pubDate, platforms: cli }, null, 2)}\n`, { flag: "wx" });
  const names = (await readdir(dir)).filter((name) => name !== "SHA256SUMS").sort();
  assert.equal(names.length, expected.length + 2, "release_checksum_count_invariant");
  const lines = [];
  for (const name of names) lines.push(`${await sha256File(join(dir, name))}  ${name}\n`);
  await writeFile(join(dir, "SHA256SUMS"), lines.join(""), { flag: "wx" });
  return OUTPUTS.map((name) => join(dir, name));
}

async function configuredPublicKey() {
  const config = JSON.parse(await readFile(join(REPO, "apps/desktop/src-tauri/tauri.conf.json"), "utf8"));
  const key = config?.plugins?.updater?.pubkey;
  if (typeof key !== "string" || key.length === 0) fail("release_public_key_missing");
  return key;
}

async function main(args) {
  const names = ["dir", "version", "channel", "commit", "base-url", "notes", "public-key-file"];
  const given = {};
  for (let index = 0; index < args.length; index += 2) {
    const name = args[index].replace(/^--/, "");
    if (!args[index].startsWith("--") || !names.includes(name) || Object.hasOwn(given, name) || index + 1 >= args.length) fail("release_arguments_rejected");
    given[name] = args[index + 1];
  }
  for (const name of ["dir", "version", "channel", "commit", "base-url"]) if (!Object.hasOwn(given, name)) fail(`release_argument_missing:--${name}`);
  const publicKey = given["public-key-file"] ? (await readFile(resolve(given["public-key-file"]), "utf8")).trim() : await configuredPublicKey();
  const written = await writeManifests({
    dir: resolve(given.dir), version: given.version, channel: given.channel, commit: given.commit,
    baseUrl: given["base-url"], notes: given.notes ?? "", pubDate: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"), publicKey,
  });
  return written.join("\n");
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    console.log(await main(process.argv.slice(2)));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
