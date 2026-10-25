import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { SECRET_NAMES, signWindows, signingPlan, signtoolArgs } from "./sign-windows.mjs";

const FULL = {
  AZURE_CLIENT_ID: "v-cid-41f7", AZURE_CLIENT_SECRET: "v-csec-93a2", AZURE_TENANT_ID: "v-tid-5c1e",
  AZURE_TRUSTED_SIGNING_ENDPOINT: "https://eus.codesigning.azure.net", AZURE_TRUSTED_SIGNING_ACCOUNT: "crazy-account", AZURE_TRUSTED_SIGNING_PROFILE: "crazy-profile",
  CRAZYAGENT_SIGNTOOL: "C:\\Kits\\signtool.exe", CRAZYAGENT_SIGNING_DLIB: "C:\\sign\\Azure.CodeSigning.Dlib.dll",
};

test("signing runs only with every Azure value and both tools, and a stable build cannot skip it", () => {
  assert.deepEqual(SECRET_NAMES, ["AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET", "AZURE_TENANT_ID", "AZURE_TRUSTED_SIGNING_ENDPOINT", "AZURE_TRUSTED_SIGNING_ACCOUNT", "AZURE_TRUSTED_SIGNING_PROFILE"]);
  assert.deepEqual(signingPlan(FULL), { mode: "sign", metadata: { Endpoint: "https://eus.codesigning.azure.net", CodeSigningAccountName: "crazy-account", CertificateProfileName: "crazy-profile" } });
  assert.deepEqual(signingPlan({}), { mode: "skip" });
  assert.throws(() => signingPlan({ CRAZYAGENT_REQUIRE_SIGNING: "1" }), /^Error: windows_signing_secrets_missing$/);
  const partial = { ...FULL };
  delete partial.AZURE_TENANT_ID;
  delete partial.AZURE_TRUSTED_SIGNING_PROFILE;
  assert.throws(() => signingPlan(partial), /^Error: windows_signing_secrets_incomplete:AZURE_TENANT_ID,AZURE_TRUSTED_SIGNING_PROFILE$/);
  const noTool = { ...FULL };
  delete noTool.CRAZYAGENT_SIGNING_DLIB;
  assert.throws(() => signingPlan(noTool), /windows_signing_tools_missing/);
  assert.throws(() => signingPlan({ ...FULL, AZURE_TRUSTED_SIGNING_ENDPOINT: "http://eus.codesigning.azure.net" }), /windows_signing_endpoint_rejected/);
  assert.throws(() => signingPlan({ ...FULL, AZURE_TRUSTED_SIGNING_ACCOUNT: "a b" }), /windows_signing_account_rejected/);
  for (const error of [() => signingPlan(partial), () => signingPlan({ ...FULL, AZURE_TRUSTED_SIGNING_ACCOUNT: "a b" })]) {
    assert.throws(error, (thrown) => ["v-cid-41f7", "v-csec-93a2", "v-tid-5c1e"].every((value) => !thrown.message.includes(value)));
  }
});

test("signtool signs with SHA-256, the Microsoft timestamp service and the Trusted Signing dlib", () => {
  assert.deepEqual(signtoolArgs({ dlib: "C:\\d\\Azure.CodeSigning.Dlib.dll", metadata: "C:\\t\\metadata.json", file: "C:\\b\\crazyagent.exe" }), [
    "sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256",
    "/dlib", "C:\\d\\Azure.CodeSigning.Dlib.dll", "/dmdf", "C:\\t\\metadata.json", "C:\\b\\crazyagent.exe",
  ]);
});

test("an unsigned dev build leaves the binary untouched and a stable build refuses it", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-winsign-"));
  try {
    const file = join(root, "crazyagent.exe");
    await writeFile(file, "MZ unsigned\n");
    assert.equal(await signWindows({ file, env: {} }), "skipped");
    assert.equal(await readFile(file, "utf8"), "MZ unsigned\n");
    await assert.rejects(signWindows({ file, env: { CRAZYAGENT_REQUIRE_SIGNING: "1" } }), /windows_signing_secrets_missing/);
    await assert.rejects(signWindows({ file: join(root, "missing.exe"), env: {} }), /windows_signing_file_missing/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
