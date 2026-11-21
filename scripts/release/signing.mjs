import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { signingPlan } from "./sign-windows.mjs";

/** Environment names the macOS signing and notarization steps read; their values are never printed. */
export const APPLE_NAMES = Object.freeze(["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY", "APPLE_TEAM_ID", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_ID", "APPLE_PASSWORD"]);

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
 * "unsigned" (a dev build without macOS secrets, or any Windows build without Azure secrets,
 * DEC-195 Q8) or "updater-only" (Linux). A stable macOS build without its secrets, and a
 * partial set of either platform's secrets, are refused.
 */
export function signingStatus({ os, channel, env }) {
  if (channel !== "dev" && channel !== "stable") fail("release_channel_rejected");
  if (!["macos", "linux", "windows"].includes(os)) fail("release_platform_rejected");
  if (!env.TAURI_SIGNING_PRIVATE_KEY && !env.TAURI_SIGNING_PRIVATE_KEY_PATH) fail("release_updater_key_missing");
  if (os === "macos") return macos(env, channel);
  if (os === "windows") return signingPlan({ ...env, CRAZYAGENT_REQUIRE_SIGNING: "0" }).mode === "sign" ? "signed" : "unsigned";
  return "updater-only";
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 4) fail("usage: signing.mjs <macos|linux|windows> <dev|stable>");
    console.log(signingStatus({ os: process.argv[2], channel: process.argv[3], env: process.env }));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
