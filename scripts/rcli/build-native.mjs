import assert from "node:assert/strict";
import { constants } from "node:fs";
import { lstat, open, readdir, realpath } from "node:fs/promises";
import { createHash } from "node:crypto";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const MAX_BYTES = 128 * 1024 * 1024;
const LOCK_SHA256 = "8844d1768585d70a827597b77d6f4f9f3203907f30779c8e7442963ebe7ef3ef";

function fail(message) {
  throw new Error(message);
}

function absolute(path) {
  if (typeof path !== "string" || !isAbsolute(path) || resolve(path) !== path || path.includes("\0")) fail("build_argv_path_rejected");
  return path;
}

function identity(stat) {
  return [stat.dev, stat.ino, stat.size, stat.mtimeNs, stat.ctimeNs].join(":");
}

export async function openPinned(path, pin) {
  absolute(path);
  if (!pin || !Number.isSafeInteger(pin.bytes) || pin.bytes < 1 || pin.bytes > MAX_BYTES || !/^[a-f0-9]{64}$/.test(pin.sha256)) fail("build_pin_invalid");
  if (await realpath(path) !== path) fail("build_path_not_canonical");
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const before = await handle.stat({ bigint: true });
    const leaf = await lstat(path, { bigint: true });
    if (!before.isFile() || before.uid !== BigInt(process.getuid()) || before.nlink !== 1n || (before.mode & 0o022n) !== 0n || before.size !== BigInt(pin.bytes) || before.dev !== leaf.dev || before.ino !== leaf.ino) fail("build_file_unsafe");
    return { path, pin, handle, before };
  } catch (error) {
    await handle.close();
    throw error;
  }
}

export async function readPinned(lease) {
  const { path, pin, handle, before } = lease;
  try {
    const currentLeaf = await lstat(path, { bigint: true });
    if (currentLeaf.dev !== before.dev || currentLeaf.ino !== before.ino) fail("build_source_replaced");
    if (identity(await handle.stat({ bigint: true })) !== identity(before)) fail("build_file_changed");
    const bytes = Buffer.alloc(pin.bytes + 1);
    let used = 0;
    while (used < bytes.length) {
      const { bytesRead } = await handle.read(bytes, used, bytes.length - used, used);
      if (bytesRead === 0) break;
      used += bytesRead;
    }
    const after = await handle.stat({ bigint: true });
    const leaf = await lstat(path, { bigint: true });
    if (leaf.dev !== before.dev || leaf.ino !== before.ino || await realpath(path) !== path) fail("build_source_replaced");
    if (identity(after) !== identity(before) || used !== pin.bytes || after.nlink !== 1n) fail("build_file_changed");
    const content = bytes.subarray(0, used);
    if (createHash("sha256").update(content).digest("hex") !== pin.sha256) fail("build_hash_mismatch");
    return content;
  } finally {
    await handle.close();
  }
}

export async function validateStaging(path) {
  absolute(path);
  const stat = await lstat(path);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o700 || await realpath(path) !== path) fail("build_staging_unsafe");
  if ((await readdir(path)).length !== 0) fail("build_staging_not_empty");
  return path;
}

export function compilerArgv(paths, target) {
  if (target?.os !== "darwin" || target.arch !== "arm64" || target.bun_target !== "bun") fail("build_target_rejected");
  for (const key of ["bun", "config", "entry", "output"]) absolute(paths[key]);
  if (paths.entry === paths.output || paths.bun === paths.output || paths.config === paths.output) fail("build_argv_path_rejected");
  return [paths.bun, "build", "--compile", "--target=bun", "--compile-executable-path=" + paths.bun, "--no-compile-autoload-dotenv", "--no-compile-autoload-bunfig", "--no-compile-autoload-tsconfig", "--no-compile-autoload-package-json", "--no-env-file", "--no-install", "--config=" + paths.config, "--outfile=" + paths.output, paths.entry];
}

async function loadLock() {
  const path = fileURLToPath(new URL("./source-lock.json", import.meta.url));
  const stat = await lstat(path);
  const bytes = await readPinned(await openPinned(path, { bytes: stat.size, sha256: LOCK_SHA256 }));
  const lock = JSON.parse(bytes.toString("utf8"));
  if (lock.schema !== 1 || lock.upstream.revision !== "51bee17e02344de5fffd89523ce2b73d5042f216") fail("build_lock_rejected");
  return lock;
}

async function contained(root, pin) {
  absolute(root);
  if (await realpath(root) !== root || typeof pin.path !== "string" || isAbsolute(pin.path) || pin.path.split("/").some(part => part === ".." || part === "." || part === "")) fail("build_source_path_rejected");
  return readPinned(await openPinned(join(root, pin.path), pin));
}

export async function inspectClosure(root, closure) {
  const groups = ["files", "assets", "licenses"];
  if (!closure || typeof closure.complete !== "boolean" ||
      !groups.every(key => Array.isArray(closure[key])) ||
      !Array.isArray(closure.missing) || closure.missing.length > 64 ||
      closure.missing.some(value => typeof value !== "string" || !value.isWellFormed() ||
        value.length === 0 || Buffer.byteLength(value) > 1024) ||
      closure.complete !== (closure.missing.length === 0) ||
      closure.files.length === 0 || closure.complete && closure.licenses.length === 0) {
    fail("build_closure_rejected");
  }
  const count = groups.reduce((total, key) => total + closure[key].length, 0);
  if (count > 4096) fail("build_closure_rejected");
  const seen = new Set();
  const counts = {};
  let bytes = 0;
  for (const key of groups) {
    counts[key] = closure[key].length;
    for (const pin of closure[key]) {
      if (!pin || typeof pin.path !== "string" || seen.has(pin.path)) {
        fail("build_closure_rejected");
      }
      if (!Number.isSafeInteger(pin.bytes) || pin.bytes < 1 || pin.bytes > MAX_BYTES) {
        fail("build_pin_invalid");
      }
      bytes += pin.bytes;
      if (bytes > MAX_BYTES) fail("build_closure_rejected");
      await contained(root, pin);
      seen.add(pin.path);
    }
  }
  assert.equal(seen.size, count, "build_closure_count_invariant");
  assert(Number.isSafeInteger(bytes) && bytes > 0, "build_closure_bytes_invariant");
  return {
    status: closure.complete ? "STATIC_INPUTS_VERIFIED" : "STATIC_INPUTS_INCOMPLETE",
    complete: closure.complete, execution_allowed: false,
    counts, bytes, missing: [...closure.missing],
  };
}

async function inspectInputs(input, lock) {
  for (const pin of lock.sdk_files) await contained(input.sdk, pin);
  for (const pin of lock.host_files) await contained(input.project, pin);
  const bun = await readPinned(await openPinned(input.bun, lock.bun));
  if (bun.readUInt32LE(0) !== 0xfeedfacf || bun.readUInt32LE(4) !== 0x0100000c) fail("build_target_rejected");
  await readPinned(await openPinned(input.config, lock.bun_config));
  const inventory = JSON.parse((await readPinned(await openPinned(input.inventory, lock.dependency_inventory))).toString("utf8"));
  if (inventory.source_revision !== lock.upstream.revision || inventory.lifecycle_scripts !== "disabled" || !Array.isArray(inventory.installed_packages) || inventory.installed_packages.length !== 176) fail("build_dependency_inventory_rejected");
  for (const item of inventory.installed_packages) {
    const path = join(input.sdk, item.path);
    const stat = await lstat(path);
    await contained(input.sdk, { path: item.path, sha256: item.package_json_sha256, bytes: stat.size });
  }
  await inspectClosure(input.sdk, lock.closure);
  const alias = join(input.project, ".local-state/tools/rcli-source");
  const metadata = await lstat(alias);
  if (!metadata.isSymbolicLink() || metadata.uid !== process.getuid() || await realpath(alias) !== input.sdk) fail("build_sdk_alias_rejected");
}

export async function planBuild(input) {
  const lock = await loadLock();
  await validateStaging(input.staging);
  if (lock.entry === null) {
    const error = new Error("build_entry_unavailable");
    error.plan = { status: "BLOCKED_MISSING_EXECUTABLE_ENTRY", argv: null, execution_allowed: false, target: lock.target, closure: lock.closure, bounds: lock.build_bounds, compiler_arguments_qualified: false, output_qualified: false };
    throw error;
  }
  if (lock.entry.executable !== true || lock.entry.path === "scripts/rcli/host.ts") fail("build_entry_contract_rejected");
  if (!lock.closure.complete || !Array.isArray(lock.closure.files) || lock.closure.files.length === 0 || !Array.isArray(lock.closure.assets) || !Array.isArray(lock.closure.licenses) || lock.closure.licenses.length === 0) fail("build_closure_unavailable");
  if (process.platform !== lock.target.os || process.arch !== lock.target.arch) fail("build_target_rejected");
  await inspectInputs(input, lock);
  await contained(input.project, lock.entry);
  await contained(input.project, lock.entry.contract);
  await validateStaging(input.staging);
  return {
    status: "INPUTS_VERIFIED_COMPILER_UNQUALIFIED",
    argv: compilerArgv({ bun: input.bun, config: input.config, entry: join(input.project, lock.entry.path), output: join(input.staging, lock.output.basename) }, lock.target),
    cwd: input.staging,
    env: { HOME: input.staging, TMPDIR: input.staging, CLAUDE_CODE_TMPDIR: input.staging },
    stdin: "closed",
    bounds: lock.build_bounds,
    required_network_policy: "deny_all",
    execution_allowed: false,
    compiler_arguments_qualified: false,
    output: lock.output,
  };
}

function parseArguments(args) {
  const names = new Map([["--sdk-root", "sdk"], ["--project-root", "project"], ["--bun", "bun"], ["--bun-config", "config"], ["--inventory", "inventory"], ["--staging-root", "staging"]]);
  const input = {};
  if (args.length !== names.size * 2) fail("build_arguments_rejected");
  for (let index = 0; index < args.length; index += 2) {
    const name = names.get(args[index]);
    if (!name || Object.hasOwn(input, name)) fail("build_arguments_rejected");
    input[name] = absolute(args[index + 1]);
  }
  return input;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    console.log(JSON.stringify(await planBuild(parseArguments(process.argv.slice(2))), null, 2));
  } catch (error) {
    console.error(JSON.stringify({ error: error.message, plan: error.plan ?? null }));
    process.exitCode = 2;
  }
}
