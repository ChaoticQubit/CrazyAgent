import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, createPublicKey, verify } from "node:crypto";
import { constants } from "node:fs";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const TAURI = join(REPO, "apps/desktop/node_modules/@tauri-apps/cli/tauri.js");
const PRODUCT = "CrazyAgent";

/** Release targets: operating system, the architecture word Tauri puts in bundle names, executable suffix. */
export const TARGETS = Object.freeze({
  "aarch64-apple-darwin": Object.freeze({ os: "macos", bundleArch: "aarch64", exe: "" }),
  "x86_64-apple-darwin": Object.freeze({ os: "macos", bundleArch: "x64", exe: "" }),
  "x86_64-unknown-linux-gnu": Object.freeze({ os: "linux", bundleArch: "amd64", exe: "" }),
  "x86_64-pc-windows-msvc": Object.freeze({ os: "windows", bundleArch: "x64", exe: ".exe" }),
});

function fail(message) {
  throw new Error(message);
}

/** Looks up a release target. Input: Rust target triple. Output: its TARGETS entry, or an error. */
export function targetInfo(target) {
  if (!Object.hasOwn(TARGETS, target)) fail("release_target_rejected");
  return TARGETS[target];
}

/**
 * Lists the executables shipped beside the desktop app and in the CLI archive for `target`:
 * the engine, the rcli host, the runner, plus the Linux Landlock or Windows boundary helper.
 */
export function sidecars(target) {
  const { os } = targetInfo(target);
  const names = ["crazyagent", "crazyagent-rcli", "crazyagent-runner"];
  if (os === "linux") names.push("crazyagent-landlock");
  if (os === "windows") names.push("crazyagent-win-boundary");
  return names;
}

/** Names the CLI archive for `version` and `target`: `.zip` on Windows, `.tar.gz` elsewhere. */
export function cliArchiveName(version, target) {
  return `crazyagent-${version}-${target}.${targetInfo(target).os === "windows" ? "zip" : "tar.gz"}`;
}

async function isFile(path) {
  try {
    return (await lstat(path)).isFile();
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: ["ignore", "pipe", "pipe"], maxBuffer: 16 * 1024 * 1024, ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) fail(`release_command_failed:${basename(command)}:${result.status}:${String(result.stderr).slice(-2000)}`);
  return result;
}

/**
 * Copies the built executables for `target` from `cargoDir` and the rcli host from `rcli` into
 * `<tauriDir>/binaries/<name>-<target>[.exe]`, and writes `binaries/release.conf.json`, the
 * `tauri build --config` overlay naming them as `bundle.externalBin`. Output: staged paths and overlay path.
 */
export async function stageSidecars({ target, cargoDir, rcli, tauriDir }) {
  const { exe } = targetInfo(target);
  const names = sidecars(target);
  const sources = names.map((name) => (name === "crazyagent-rcli" ? rcli : join(cargoDir, `${name}${exe}`)));
  for (const [index, source] of sources.entries()) if (!(await isFile(source))) fail(`release_sidecar_missing:${names[index]}`);
  const out = join(tauriDir, "binaries");
  await mkdir(out, { recursive: true });
  const staged = [];
  for (const [index, name] of names.entries()) {
    const path = join(out, `${name}-${target}${exe}`);
    await rm(path, { force: true });
    await copyFile(sources[index], path, constants.COPYFILE_EXCL);
    await chmod(path, 0o755);
    staged.push(path);
  }
  const config = join(out, "release.conf.json");
  await writeFile(config, `${JSON.stringify({ bundle: { externalBin: names.map((name) => `binaries/${name}`) } }, null, 2)}\n`);
  assert.equal(staged.length, names.length, "release_stage_count_invariant");
  return { staged, config };
}

/**
 * Packs the target's executables from `source` (named `<name>-<target>` when `suffixed`, else
 * `<name>`) into `<out>/crazyagent-<version>-<target>.tar.gz` (`.zip` on Windows), with every
 * executable at the archive root. Output: the archive path.
 */
export async function packageCli({ target, version, source, suffixed, out }) {
  const { os, exe } = targetInfo(target);
  const names = sidecars(target);
  await mkdir(out, { recursive: true });
  const archive = join(out, cliArchiveName(version, target));
  if (await isFile(archive)) fail(`release_asset_exists:${basename(archive)}`);
  const stage = await mkdtemp(join(tmpdir(), "crazyagent-cli-"));
  try {
    const members = [];
    for (const name of names) {
      const from = join(source, suffixed ? `${name}-${target}${exe}` : `${name}${exe}`);
      if (!(await isFile(from))) fail(`release_sidecar_missing:${name}`);
      await copyFile(from, join(stage, `${name}${exe}`), constants.COPYFILE_EXCL);
      await chmod(join(stage, `${name}${exe}`), 0o755);
      members.push(`${name}${exe}`);
    }
    if (os === "windows") {
      const tar = process.platform === "win32" ? join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe") : "tar";
      run(tar, ["-a", "-c", "-f", archive, "-C", stage, ...members]);
    } else {
      run("tar", ["--no-xattrs", "-c", "-z", "-f", archive, "-C", stage, ...members], { env: { ...process.env, COPYFILE_DISABLE: "1" } });
    }
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
  if (!(await isFile(archive))) fail("release_archive_not_written");
  return archive;
}

/**
 * Signs `file` with the Tauri updater key from `env` (TAURI_SIGNING_PRIVATE_KEY or
 * TAURI_SIGNING_PRIVATE_KEY_PATH, plus TAURI_SIGNING_PRIVATE_KEY_PASSWORD), binding `version`
 * into the trusted comment. The key never reaches the command line. Output: `<file>.sig`.
 */
export async function signFile({ file, version, env = process.env, tauri = TAURI }) {
  if (!env.TAURI_SIGNING_PRIVATE_KEY && !env.TAURI_SIGNING_PRIVATE_KEY_PATH) fail("release_signing_key_missing");
  const signature = `${file}.sig`;
  if (await isFile(signature)) fail(`release_asset_exists:${basename(signature)}`);
  run(process.execPath, [tauri, "signer", "sign", "--app-version", version, file], { env });
  if (!(await isFile(signature))) fail("release_signature_not_written");
  return signature;
}

function boxLines(encoded, label) {
  const text = Buffer.from(String(encoded).trim(), "base64").toString("utf8");
  const lines = text.split("\n");
  while (lines.at(-1) === "") lines.pop();
  if (!lines.every((line) => !line.includes("\r")) || !lines[0]?.startsWith("untrusted comment: ")) fail(`release_${label}_rejected`);
  return lines;
}

/**
 * Verifies a Tauri `.sig` (base64 minisign signature) over `file` against the base64 Tauri
 * `publicKey`, including the global signature over the trusted comment, and requires the signed
 * version to equal `version`. Output: the signed `version` and `file` name.
 */
export async function verifySignature({ file, signature, publicKey, version }) {
  const keyLines = boxLines(publicKey, "public_key");
  const key = keyLines.length === 2 ? Buffer.from(keyLines[1], "base64") : Buffer.alloc(0);
  if (key.length !== 42 || key.subarray(0, 2).toString("latin1") !== "Ed") fail("release_public_key_rejected");
  const lines = boxLines(signature, "signature");
  if (lines.length !== 4 || !lines[2].startsWith("trusted comment: ")) fail("release_signature_rejected");
  const box = Buffer.from(lines[1], "base64");
  const algorithm = box.subarray(0, 2).toString("latin1");
  if (box.length !== 74 || (algorithm !== "ED" && algorithm !== "Ed")) fail("release_signature_rejected");
  if (!box.subarray(2, 10).equals(key.subarray(2, 10))) fail("release_signature_key_mismatch");
  const publicKeyObject = createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: key.subarray(10).toString("base64url") }, format: "jwk" });
  const data = await readFile(file);
  const message = algorithm === "ED" ? createHash("blake2b512").update(data).digest() : data;
  if (!verify(null, message, publicKeyObject, box.subarray(10))) fail("release_signature_invalid");
  const trusted = lines[2].slice("trusted comment: ".length);
  const global = Buffer.from(lines[3], "base64");
  if (global.length !== 64 || !verify(null, Buffer.concat([box.subarray(10), Buffer.from(trusted, "utf8")]), publicKeyObject, global)) fail("release_signature_invalid");
  const fields = new Map(trusted.split("\t").map((field) => [field.slice(0, field.indexOf(":")), field.slice(field.indexOf(":") + 1)]));
  if (fields.get("version") !== version) fail("release_signature_version_mismatch");
  return { version: fields.get("version"), file: fields.get("file") };
}

/**
 * Lists the desktop bundles Tauri writes for `target` and `version` under its bundle folder, the
 * release asset name of each and whether Tauri signs it as an updater artifact.
 */
export function desktopLayout(target, version) {
  const { os, bundleArch } = targetInfo(target);
  const stem = `${PRODUCT}_${version}_${bundleArch}`;
  if (os === "macos") return [
    { from: `macos/${PRODUCT}.app.tar.gz`, to: `${stem}.app.tar.gz`, updater: true },
    { from: `dmg/${stem}.dmg`, to: `${stem}.dmg`, updater: false },
  ];
  if (os === "linux") return [
    { from: `appimage/${stem}.AppImage`, to: `${stem}.AppImage`, updater: true },
    { from: `deb/${stem}.deb`, to: `${stem}.deb`, updater: true },
  ];
  return [{ from: `nsis/${stem}-setup.exe`, to: `${stem}-setup.exe`, updater: true }];
}

/**
 * Copies the target's desktop bundles from `bundleDir` into `out` under release names, with
 * Tauri's updater signature beside each updater artifact and a new signature for the others.
 * Output: the asset paths in layout order.
 */
export async function collectDesktop({ target, version, bundleDir, out, env = process.env, tauri = TAURI }) {
  const layout = desktopLayout(target, version);
  for (const item of layout) {
    if (!(await isFile(join(bundleDir, item.from)))) fail(`release_bundle_missing:${item.from}`);
    if (item.updater && !(await isFile(join(bundleDir, `${item.from}.sig`)))) fail(`release_updater_signature_missing:${item.from}`);
  }
  await mkdir(out, { recursive: true });
  const assets = [];
  for (const item of layout) {
    const to = join(out, item.to);
    await copyFile(join(bundleDir, item.from), to, constants.COPYFILE_EXCL);
    if (item.updater) await copyFile(join(bundleDir, `${item.from}.sig`), `${to}.sig`, constants.COPYFILE_EXCL);
    else await signFile({ file: to, version, env, tauri });
    assets.push(to);
  }
  assert.equal(assets.length, layout.length, "release_collect_count_invariant");
  return assets;
}

function options(args, names, flags = []) {
  const parsed = {};
  for (let index = 0; index < args.length; index += 1) {
    const name = args[index].replace(/^--/, "");
    if (!args[index].startsWith("--") || Object.hasOwn(parsed, name)) fail("release_arguments_rejected");
    if (flags.includes(name)) parsed[name] = true;
    else if (names.includes(name) && index + 1 < args.length) parsed[name] = args[++index];
    else fail("release_arguments_rejected");
  }
  for (const name of names) if (!Object.hasOwn(parsed, name)) fail(`release_argument_missing:--${name}`);
  return parsed;
}

async function main([command, ...args]) {
  if (command === "stage") {
    const given = options(args, ["target", "cargo-dir", "rcli", "tauri-dir"]);
    return (await stageSidecars({ target: given.target, cargoDir: resolve(given["cargo-dir"]), rcli: resolve(given.rcli), tauriDir: resolve(given["tauri-dir"]) })).config;
  }
  if (command === "package") {
    const given = options(args, ["target", "version", "source", "out"], ["suffixed", "sign"]);
    const archive = await packageCli({ target: given.target, version: given.version, source: resolve(given.source), suffixed: given.suffixed === true, out: resolve(given.out) });
    if (given.sign) await signFile({ file: archive, version: given.version });
    return archive;
  }
  if (command === "collect") {
    const given = options(args, ["target", "version", "bundle-dir", "out"]);
    return (await collectDesktop({ target: given.target, version: given.version, bundleDir: resolve(given["bundle-dir"]), out: resolve(given.out) })).join("\n");
  }
  fail("usage: assets.mjs stage|package|collect --target <triple> ...");
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    console.log(await main(process.argv.slice(2)));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
