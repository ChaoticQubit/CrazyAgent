import assert from "node:assert/strict";
import test from "node:test";
import { APPLE_NAMES, signingStatus } from "./signing.mjs";

const UPDATER = { TAURI_SIGNING_PRIVATE_KEY: "v-key-77", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "v-pw-12" };
const APPLE = { APPLE_CERTIFICATE: "v-cert-1", APPLE_CERTIFICATE_PASSWORD: "v-cpw-2", APPLE_SIGNING_IDENTITY: "Developer ID Application: Owner (TEAM123456)" };
const API = { APPLE_API_KEY: "KEY1234567", APPLE_API_ISSUER: "v-issuer-3", APPLE_API_KEY_PATH: "/tmp/AuthKey.p8" };
const ID = { APPLE_ID: "owner@example.test", APPLE_PASSWORD: "v-app-pw-4", APPLE_TEAM_ID: "TEAM123456" };
const AZURE = {
  AZURE_CLIENT_ID: "v-cid", AZURE_CLIENT_SECRET: "v-csec", AZURE_TENANT_ID: "v-tid",
  AZURE_TRUSTED_SIGNING_ENDPOINT: "https://eus.codesigning.azure.net", AZURE_TRUSTED_SIGNING_ACCOUNT: "acct", AZURE_TRUSTED_SIGNING_PROFILE: "prof",
  CRAZYAGENT_SIGNTOOL: "C:\\signtool.exe", CRAZYAGENT_SIGNING_DLIB: "C:\\Azure.CodeSigning.Dlib.dll",
};

test("every channel needs the updater key, because updates are verified against it", () => {
  for (const os of ["macos", "linux", "windows"]) {
    for (const channel of ["dev", "stable"]) assert.throws(() => signingStatus({ os, channel, env: {} }), /release_updater_key_missing/);
  }
  assert.equal(signingStatus({ os: "linux", channel: "stable", env: UPDATER }), "updater-only");
  assert.throws(() => signingStatus({ os: "freebsd", channel: "dev", env: UPDATER }), /release_platform_rejected/);
  assert.throws(() => signingStatus({ os: "linux", channel: "nightly", env: UPDATER }), /release_channel_rejected/);
});

test("macOS signs with a Developer ID and notarizes with an API key or an Apple ID", () => {
  assert.deepEqual(APPLE_NAMES, ["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY", "APPLE_TEAM_ID", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_ID", "APPLE_PASSWORD"]);
  assert.equal(signingStatus({ os: "macos", channel: "stable", env: { ...UPDATER, ...APPLE, ...API } }), "signed");
  assert.equal(signingStatus({ os: "macos", channel: "stable", env: { ...UPDATER, ...APPLE, ...ID } }), "signed");
  assert.equal(signingStatus({ os: "macos", channel: "dev", env: UPDATER }), "unsigned");
  assert.throws(() => signingStatus({ os: "macos", channel: "stable", env: UPDATER }), /^Error: macos_signing_secrets_missing$/);
  assert.throws(() => signingStatus({ os: "macos", channel: "dev", env: { ...UPDATER, ...APPLE } }), /^Error: macos_notarization_secrets_missing$/);
  assert.throws(() => signingStatus({ os: "macos", channel: "dev", env: { ...UPDATER, APPLE_CERTIFICATE: "v-cert-1", ...API } }), /^Error: macos_signing_secrets_incomplete:APPLE_CERTIFICATE_PASSWORD,APPLE_SIGNING_IDENTITY$/);
  assert.throws(() => signingStatus({ os: "macos", channel: "dev", env: { ...UPDATER, ...APPLE, APPLE_API_KEY: "KEY1234567" } }), /^Error: macos_notarization_secrets_missing$/);
});

test("Windows signs with Azure Trusted Signing and only a dev build may stay unsigned", () => {
  assert.equal(signingStatus({ os: "windows", channel: "stable", env: { ...UPDATER, ...AZURE } }), "signed");
  assert.equal(signingStatus({ os: "windows", channel: "dev", env: UPDATER }), "unsigned");
  assert.throws(() => signingStatus({ os: "windows", channel: "stable", env: UPDATER }), /windows_signing_secrets_missing/);
});

test("no refusal carries a secret value", () => {
  const cases = [
    () => signingStatus({ os: "macos", channel: "dev", env: { ...UPDATER, APPLE_CERTIFICATE: "v-cert-1", ...API } }),
    () => signingStatus({ os: "windows", channel: "dev", env: { ...UPDATER, AZURE_CLIENT_SECRET: "v-csec" } }),
  ];
  for (const run of cases) assert.throws(run, (error) => ["v-key-77", "v-pw-12", "v-cert-1", "v-issuer-3", "v-csec"].every((value) => !error.message.includes(value)));
});
