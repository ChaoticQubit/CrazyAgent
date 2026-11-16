import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { basename, isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { MACOS_PRESIGNED, isFile, targetInfo } from "./assets.mjs";
import { signingPlan } from "./sign-windows.mjs";

/** Environment names the macOS signing and notarization steps read; their values are never printed. */
export const APPLE_NAMES = Object.freeze(["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY", "APPLE_TEAM_ID", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_ID", "APPLE_PASSWORD"]);

const CODESIGN = "/usr/bin/codesign";
const CODESIGN_MS_MAX = 300_000;
const STDERR_TAIL = 1500;
const PRESIGN_USAGE = "usage: signing.mjs presign --target <triple> --tauri-dir <dir> --keychain <path>";

function fail(message) {
  throw new Error(message);
}

function macos(env, channel) {
  const identity = ["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY"];
  const missing = identity.filter((name) => !env[name]);
  if (missing.length === identity.length) {
    if (channel === "stable") fail("macos_signing_secrets_missing");
    return "unsigned";
  }
  if (missing.length !== 0) fail(`macos_signing_secrets_incomplete:${missing.join(",")}`);
  const api = ["APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH"].every((name) => env[name]);
  const appleId = ["APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID"].every((name) => env[name]);
  if (!api && !appleId) fail("macos_notarization_secrets_missing");
  return "signed";
}

/**
 * Decides how a release build on `os` ("macos", "linux", "windows") for `channel` ("dev",
 * "stable") is signed from `env`. The Tauri updater key is always required. Output: "signed",
 * "unsigned" (dev build without platform signing secrets) or "updater-only" (Linux).
 * A stable build with missing or partial platform secrets is refused.
 */
export function signingStatus({ os, channel, env }) {
  if (channel !== "dev" && channel !== "stable") fail("release_channel_rejected");
  if (!["macos", "linux", "windows"].includes(os)) fail("release_platform_rejected");
  if (!env.TAURI_SIGNING_PRIVATE_KEY && !env.TAURI_SIGNING_PRIVATE_KEY_PATH) fail("release_updater_key_missing");
  if (os === "macos") return macos(env, channel);
  if (os === "windows") return signingPlan({ ...env, CRAZYAGENT_REQUIRE_SIGNING: channel === "stable" ? "1" : "0" }).mode === "sign" ? "signed" : "unsigned";
  return "updater-only";
}

/**
 * Builds the codesign arguments that sign one staged macOS executable with the Developer ID
 * `identity` from `keychain`: hardened runtime, secure timestamp, `identifier` (one of
 * MACOS_PRESIGNED) as the code identifier, and no entitlements. Inputs: absolute `keychain` and
 * `file` paths. Output: the argument list.
 */
export function macosPresignArgs({ identity, keychain, identifier, file }) {
  if (!MACOS_PRESIGNED.includes(identifier)) fail("macos_presign_identifier_rejected");
  assert(typeof identity === "string" && identity !== "", "macos_presign_identity_invariant");
  assert(isAbsolute(keychain) && isAbsolute(file), "macos_presign_path_invariant");
  return ["--force", "--options", "runtime", "--timestamp", "--identifier", identifier, "--keychain", keychain, "--sign", identity, file];
}

/**
 * Signs the staged engine and lid helper, `<tauriDir>/binaries/<name>-<target>`, for a macOS
 * `target` with APPLE_SIGNING_IDENTITY from `env`, before Tauri copies them into the bundle and
 * seals it. codesign gets only PATH and HOME, never a secret, and no refusal names the identity.
 * Output: the signed paths in MACOS_PRESIGNED order.
 */
export async function presignMacos({ target, tauriDir, keychain, env = process.env, codesign = CODESIGN }) {
  if (targetInfo(target).os !== "macos") fail("macos_presign_target_rejected");
  const identity = env.APPLE_SIGNING_IDENTITY;
  if (!identity) fail("macos_presign_identity_missing");
  const files = MACOS_PRESIGNED.map((name) => join(tauriDir, "binaries", `${name}-${target}`));
  for (const [index, file] of files.entries()) if (!(await isFile(file))) fail(`macos_presign_file_missing:${MACOS_PRESIGNED[index]}`);
  const childEnv = env.HOME ? { PATH: "/usr/bin:/bin", HOME: env.HOME } : { PATH: "/usr/bin:/bin" };
  for (const [index, file] of files.entries()) {
    const args = macosPresignArgs({ identity, keychain, identifier: MACOS_PRESIGNED[index], file });
    const result = spawnSync(codesign, args, { env: childEnv, stdio: ["ignore", "ignore", "pipe"], encoding: "utf8", timeout: CODESIGN_MS_MAX });
    if (result.error) throw result.error;
    if (result.status !== 0) fail(`macos_presign_failed:${basename(file)}:${result.status}:${String(result.stderr).split(identity).join("<identity>").slice(-STDERR_TAIL)}`);
  }
  assert.equal(files.length, MACOS_PRESIGNED.length, "macos_presign_count_invariant");
  return files;
}

function presignArguments(args) {
  if (args.length !== 6) fail(PRESIGN_USAGE);
  const [target, tauriDir, keychain] = ["--target", "--tauri-dir", "--keychain"].map((flag, index) => {
    if (args[index * 2] !== flag) fail(PRESIGN_USAGE);
    return args[index * 2 + 1];
  });
  return { target, tauriDir: resolve(tauriDir), keychain: resolve(keychain) };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const args = process.argv.slice(2);
    if (args[0] === "presign") {
      console.log((await presignMacos(presignArguments(args.slice(1)))).map((path) => basename(path)).join("\n"));
    } else {
      if (args.length !== 2) fail("usage: signing.mjs <macos|linux|windows> <dev|stable>");
      console.log(signingStatus({ os: args[0], channel: args[1], env: process.env }));
    }
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
