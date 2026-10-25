import { spawnSync } from "node:child_process";
import { lstat, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** Environment names that carry the Azure Trusted Signing identity; their values are never printed. */
export const SECRET_NAMES = Object.freeze(["AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET", "AZURE_TENANT_ID", "AZURE_TRUSTED_SIGNING_ENDPOINT", "AZURE_TRUSTED_SIGNING_ACCOUNT", "AZURE_TRUSTED_SIGNING_PROFILE"]);

function fail(message) {
  throw new Error(message);
}

/**
 * Decides how to sign from `env`: "sign" with the Trusted Signing metadata when every Azure value
 * and both tool paths (CRAZYAGENT_SIGNTOOL, CRAZYAGENT_SIGNING_DLIB) are set, "skip" when none is
 * set and CRAZYAGENT_REQUIRE_SIGNING is not "1", otherwise an error naming only what is missing.
 */
export function signingPlan(env) {
  const missing = SECRET_NAMES.filter((name) => !env[name]);
  if (missing.length === SECRET_NAMES.length) {
    if (env.CRAZYAGENT_REQUIRE_SIGNING === "1") fail("windows_signing_secrets_missing");
    return { mode: "skip" };
  }
  if (missing.length !== 0) fail(`windows_signing_secrets_incomplete:${missing.join(",")}`);
  if (!env.CRAZYAGENT_SIGNTOOL || !env.CRAZYAGENT_SIGNING_DLIB) fail("windows_signing_tools_missing");
  if (!/^https:\/\/[a-z0-9.-]+\.codesigning\.azure\.net\/?$/.test(env.AZURE_TRUSTED_SIGNING_ENDPOINT)) fail("windows_signing_endpoint_rejected");
  if (!/^[A-Za-z0-9-]{1,64}$/.test(env.AZURE_TRUSTED_SIGNING_ACCOUNT)) fail("windows_signing_account_rejected");
  if (!/^[A-Za-z0-9-]{1,64}$/.test(env.AZURE_TRUSTED_SIGNING_PROFILE)) fail("windows_signing_profile_rejected");
  return {
    mode: "sign",
    metadata: { Endpoint: env.AZURE_TRUSTED_SIGNING_ENDPOINT, CodeSigningAccountName: env.AZURE_TRUSTED_SIGNING_ACCOUNT, CertificateProfileName: env.AZURE_TRUSTED_SIGNING_PROFILE },
  };
}

/** Builds the signtool arguments that sign `file` through the Trusted Signing `dlib` and `metadata` file. */
export function signtoolArgs({ dlib, metadata, file }) {
  return ["sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256", "/dlib", dlib, "/dmdf", metadata, file];
}

/**
 * Signs the Windows executable `file` with Azure Trusted Signing when `env` holds the signing
 * identity; the dlib reads AZURE_CLIENT_ID, AZURE_CLIENT_SECRET and AZURE_TENANT_ID itself.
 * Output: "signed" or "skipped" (unsigned dev build).
 */
export async function signWindows({ file, env = process.env }) {
  try {
    if (!(await lstat(file)).isFile()) fail("windows_signing_file_missing");
  } catch (error) {
    if (error.code === "ENOENT") fail("windows_signing_file_missing");
    throw error;
  }
  const plan = signingPlan(env);
  if (plan.mode === "skip") return "skipped";
  const work = await mkdtemp(join(tmpdir(), "crazyagent-sign-"));
  try {
    const metadata = join(work, "metadata.json");
    await writeFile(metadata, JSON.stringify(plan.metadata), { mode: 0o600 });
    const result = spawnSync(env.CRAZYAGENT_SIGNTOOL, signtoolArgs({ dlib: env.CRAZYAGENT_SIGNING_DLIB, metadata, file }), { env, stdio: ["ignore", "pipe", "pipe"], timeout: 300_000 });
    if (result.error) throw result.error;
    if (result.status !== 0) fail(`windows_signing_failed:${basename(file)}:${result.status}:${String(result.stdout).slice(-1500)}${String(result.stderr).slice(-1500)}`);
  } finally {
    await rm(work, { recursive: true, force: true });
  }
  return "signed";
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 3) fail("usage: sign-windows.mjs <file>");
    const file = resolve(process.argv[2]);
    console.error(`${await signWindows({ file })}: ${basename(file)}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
