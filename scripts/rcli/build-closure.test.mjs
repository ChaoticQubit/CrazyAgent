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
