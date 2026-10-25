import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, mkdir, mkdtemp, readFile, realpath, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));

function fail(message) {
  throw new Error(message);
}

/**
 * Builds the two bun command lines for the rcli host: bundle `entry` into `<work>/bundle/entry.js`,
 * then compile that bundle with the same bun runtime into `output`. Output: [bundle argv, compile argv].
 */
export function rcliCommands({ bun, entry, work, output }) {
  const common = ["--packages=bundle", "--reject-unresolved", "--env=disable"];
  return [
    [bun, "build", entry, "--target=bun", "--format=esm", ...common, `--outdir=${join(work, "bundle")}`, "--entry-naming=entry.js"],
    [bun, "build", join(work, "bundle", "entry.js"), "--compile", `--compile-executable-path=${bun}`, "--target=bun", ...common, `--outfile=${output}`],
  ];
}

function isolatedEnv(work) {
  const env = { HOME: work, TMPDIR: work, LANG: "C", NODE_ENV: "production", PATH: process.env.PATH ?? "" };
  if (process.platform === "win32") Object.assign(env, { SystemRoot: process.env.SystemRoot, USERPROFILE: work, TEMP: work, TMP: work });
  return env;
}

function run(argv, options) {
  const result = spawnSync(argv[0], argv.slice(1), { stdio: ["ignore", "pipe", "pipe"], maxBuffer: 16 * 1024 * 1024, ...options });
  if (result.error) throw result.error;
  return result;
}

async function aliasSdk(repo, sdk) {
  const alias = join(repo, ".local-state", "tools", "rcli-source");
  const target = await realpath(sdk);
  let existing = null;
  try {
    existing = await lstat(alias);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (existing === null) {
    await mkdir(dirname(alias), { recursive: true });
    await symlink(target, alias, "dir");
  } else if (await realpath(alias) !== target) {
    fail("release_rcli_sdk_alias_rejected");
  }
}

/**
 * Builds `crazyagent-rcli` from scripts/rcli/entry.ts against the pinned Backboard-R-CLI checkout
 * `sdk` (with installed node_modules) using `bun`, with HOME and TMPDIR in a private work folder,
 * then runs the result once with no arguments, which must refuse with exit 70.
 * Output: path, size and SHA-256 of the executable.
 */
export async function buildRcli({ repo = REPO, sdk, bun, output }) {
  for (const path of [sdk, bun, output]) if (typeof path !== "string" || !isAbsolute(path)) fail("release_rcli_path_rejected");
  const lock = JSON.parse(await readFile(join(REPO, "scripts", "rcli", "source-lock.json"), "utf8"));
  await aliasSdk(repo, sdk);
  const manifest = JSON.parse(await readFile(join(sdk, "package.json"), "utf8"));
  if (manifest.name !== lock.upstream.package || manifest.version !== lock.upstream.version) fail("release_rcli_sdk_version_rejected");
  if (!(await lstat(join(sdk, "node_modules"))).isDirectory()) fail("release_rcli_sdk_dependencies_missing");
  const work = await realpath(await mkdtemp(join(tmpdir(), "crazyagent-rcli-")));
  try {
    await mkdir(join(work, "home"));
    const env = isolatedEnv(join(work, "home"));
    for (const argv of rcliCommands({ bun, entry: join(repo, "scripts", "rcli", "entry.ts"), work, output })) {
      const result = run(argv, { cwd: work, env, timeout: 600_000 });
      if (result.status !== 0) fail(`release_rcli_build_failed:${result.status}:${String(result.stderr).slice(-2000)}`);
    }
    const smoke = run([output], { cwd: work, env, timeout: 60_000 });
    if (smoke.status !== 70 || String(smoke.stderr) !== "host_execution_failed\n") fail(`release_rcli_smoke_failed:${smoke.status}`);
  } finally {
    await rm(work, { recursive: true, force: true });
  }
  const bytes = await readFile(output);
  assert(bytes.length > 0, "release_rcli_output_invariant");
  return { output, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") };
}

async function main(args) {
  const names = new Map([["--sdk", "sdk"], ["--bun", "bun"], ["--output", "output"]]);
  const given = {};
  if (args.length !== names.size * 2) fail("usage: rcli.mjs --sdk <checkout> --bun <bun> --output <file>");
  for (let index = 0; index < args.length; index += 2) {
    const name = names.get(args[index]);
    if (!name || Object.hasOwn(given, name)) fail("release_arguments_rejected");
    given[name] = resolve(args[index + 1]);
  }
  return JSON.stringify(await buildRcli(given));
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    console.log(await main(process.argv.slice(2)));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
