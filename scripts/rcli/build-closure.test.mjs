import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmod, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { inspectClosure } from "./build-native.mjs";

async function inputs() {
  const root = await realpath(await mkdtemp(join(tmpdir(), "rcli-closure-")));
  await chmod(root, 0o700);
  const pin = async (path, value) => {
    const content = Buffer.from(value);
    await writeFile(join(root, path), content, { mode: 0o600 });
    return { path, bytes: content.length, sha256: createHash("sha256").update(content).digest("hex") };
  };
  const source = await pin("unit.mjs", "export const value = 1;\n");
  const asset = await pin("unit.txt", "unit asset\n");
  const license = await pin("LICENSE", "unit license input data\n");
  return { root, closure: { complete: false, files: [source], assets: [asset], licenses: [license], missing: ["runtime notices unresolved"] } };
}

test("known closure files are verified while unresolved native admission stays closed", async () => {
  const { root, closure } = await inputs();
  try {
    assert.deepEqual(await inspectClosure(root, closure), {
      status: "STATIC_INPUTS_INCOMPLETE", complete: false, execution_allowed: false,
      counts: { files: 1, assets: 1, licenses: 1 },
      bytes: closure.files[0].bytes + closure.assets[0].bytes + closure.licenses[0].bytes,
      missing: ["runtime notices unresolved"],
    });
    const result = await inspectClosure(root, { ...closure, complete: true, missing: [] });
    assert.equal(result.status, "STATIC_INPUTS_VERIFIED");
    assert.equal(result.execution_allowed, false);
  } finally { await rm(root, { recursive: true }); }
});

test("actual source asset and license mutation cannot retain closure acceptance", async () => {
  const { root, closure } = await inputs();
  try {
    for (const pin of [...closure.files, ...closure.assets, ...closure.licenses]) {
      const path = join(root, pin.path);
      const bytes = await readFile(path);
      await writeFile(path, Buffer.alloc(bytes.length, 120));
      await assert.rejects(inspectClosure(root, closure), /build_hash_mismatch/);
      await writeFile(path, bytes);
    }
  } finally { await rm(root, { recursive: true }); }
});

test("contradictory complete metadata duplicates and unsafe paths fail closed", async () => {
  const { root, closure } = await inputs();
  try {
    for (const value of [
      { ...closure, complete: true },
      { ...closure, complete: true, missing: [], licenses: [] },
      { ...closure, complete: "true" },
      { ...closure, missing: [] },
      { ...closure, missing: [""] },
      { ...closure, missing: Array(65).fill("gap") },
      { ...closure, assets: closure.files },
      { ...closure, files: Array(4097).fill(closure.files[0]) },
    ]) await assert.rejects(inspectClosure(root, value), /build_closure_rejected/);
    await assert.rejects(inspectClosure(root, { ...closure, files: [{ ...closure.files[0], path: "../outside" }] }), /build_source_path_rejected/);
    const alias = join(root, "alias.mjs");
    await symlink(join(root, "unit.mjs"), alias);
    await assert.rejects(inspectClosure(root, { ...closure, files: [{ ...closure.files[0], path: "alias.mjs" }] }), /build_path_not_canonical/);
    assert.equal((await readFile(join(root, "unit.mjs"), "utf8")), "export const value = 1;\n");
  } finally { await rm(root, { recursive: true }); }
});

test("mutation of trusted manifest ownership trips the asynchronous count invariant", async () => {
  const { root, closure } = await inputs();
  try {
    const bytes = Buffer.from("another unit asset\n");
    await writeFile(join(root, "another.txt"), bytes, { mode: 0o600 });
    const pending = inspectClosure(root, closure);
    closure.assets.push({ path: "another.txt", bytes: bytes.length,
      sha256: createHash("sha256").update(bytes).digest("hex") });
    await assert.rejects(pending, /build_closure_count_invariant/);
  } finally { await rm(root, { recursive: true }); }
});

test("current host pins verify actual project files without admitting execution", async () => {
  const root = await realpath(new URL("../../", import.meta.url));
  const lock = JSON.parse(await readFile(new URL("./source-lock.json", import.meta.url), "utf8"));
  const result = await inspectClosure(root, {
    complete: false, files: lock.host_files, assets: [], licenses: [], missing: lock.closure.missing,
  });
  assert.equal(result.status, "STATIC_INPUTS_INCOMPLETE");
  assert.equal(result.execution_allowed, false);
  assert.deepEqual(lock.host_files.map(({ path }) => path).sort(), [
    "scripts/rcli/entry.ts",
    "scripts/rcli/host.runtime.ts",
    "scripts/rcli/host.ts",
    "scripts/rcli/host.codec.ts",
    "scripts/rcli/host.turns.ts",
    "scripts/rcli/host.messages.ts",
    "scripts/rcli/host.observations.ts",
    "scripts/rcli/host.test.ts",
    "scripts/rcli/host.construction.test.ts",
    "scripts/rcli/host.keys.ts",
  ].sort());
});

test("actual executable source entry and contract pins remain incomplete and nonexecuting", async () => {
  const root = await realpath(new URL("../../", import.meta.url));
  const lock = JSON.parse(await readFile(new URL("./source-lock.json", import.meta.url), "utf8"));
  assert.notEqual(lock.entry, null, "actual source-entry descriptor is required");
  assert.deepEqual(lock.entry, {
    path: "scripts/rcli/entry.ts",
    sha256: "93f81817b0eae962d767a1bc8a26958d7f12dc6b7660e41ac1d12b113e588156",
    bytes: 489,
    executable: true,
    contract: {
      path: "scripts/rcli/host.entry.test.mjs",
      sha256: "3b7ea0b7230dc5f1430ff35b2b70b92cfbe118e748192eb3d982051f7855b48d",
      bytes: 10195,
    },
  });
  assert.equal(lock.closure.complete, false);
  assert.equal(lock.output.qualified, false);
  assert.equal(lock.compiler_arguments_qualified, false);
  const result = await inspectClosure(root, {
    complete: false,
    files: [lock.entry, lock.entry.contract],
    assets: [],
    licenses: [],
    missing: lock.closure.missing,
  });
  assert.deepEqual(result, {
    status: "STATIC_INPUTS_INCOMPLETE",
    complete: false,
    execution_allowed: false,
    counts: { files: 2, assets: 0, licenses: 0 },
    bytes: 10684,
    missing: [
      "complete Bun compiler import/asset resolution proof",
      "complete version-bound Bun linked/embedded component notices and LGPL redistribution/relinking materials",
      "current native-entry admission contracts and host-file pins",
    ],
  });
});

test("actual source entry and contract each reject a corrupted descriptor digest", async () => {
  const root = await realpath(new URL("../../", import.meta.url));
  const lock = JSON.parse(await readFile(new URL("./source-lock.json", import.meta.url), "utf8"));
  assert.notEqual(lock.entry, null, "actual source-entry descriptor is required");
  const pins = [lock.entry, lock.entry.contract];
  for (const index of [0, 1]) {
    const files = pins.map((pin, position) => position === index
      ? { ...pin, sha256: "0".repeat(64) }
      : pin);
    await assert.rejects(inspectClosure(root, {
      complete: false,
      files,
      assets: [],
      licenses: [],
      missing: lock.closure.missing,
    }), /build_hash_mismatch/);
  }
});

test("real source entry leaves the planner closed and private empty staging unchanged", async () => {
  const { planBuild } = await import("./build-native.mjs");
  const { lstat, readdir } = await import("node:fs/promises");
  const staging = await realpath(await mkdtemp(join(tmpdir(), "rcli-source-entry-")));
  await chmod(staging, 0o700);
  const before = await lstat(staging);
  const entries = await readdir(staging);
  const fields = ["dev", "ino", "mode", "nlink", "uid", "gid", "size", "mtimeMs", "ctimeMs"];
  try {
    assert.equal(before.mode & 0o777, 0o700);
    assert.deepEqual(entries, []);
    await assert.rejects(planBuild({ staging }), { message: "build_closure_unavailable" });
  } finally {
    try {
      const after = await lstat(staging);
      assert.deepEqual(await readdir(staging), entries);
      assert.deepEqual(fields.map(field => after[field]), fields.map(field => before[field]));
    } finally {
      await rm(staging, { recursive: true, force: true });
    }
  }
});
