import assert from "node:assert/strict";
import test from "node:test";
import { lstatSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createRunDirectories } from "./run-layout.mjs";
import { createNavigationReader } from "./refresh-barrier.mjs";

test("shared runner creates private owned home tmp and webdriver under umask 0022", () => {
  const mask = process.umask(0o022);
  const evidence = mkdtempSync(join(tmpdir(), "ca-run-layout-"));
  try {
    createRunDirectories(evidence);
    for (const name of ["home", "tmp", "webdriver"]) {
      const info = lstatSync(join(evidence, name));
      console.log(JSON.stringify({ directory: name, mode: (info.mode & 0o777).toString(8),
        uid: info.uid, symlink: info.isSymbolicLink(), umask: process.umask().toString(8) }));
      assert(info.isDirectory());
      assert.equal(info.isSymbolicLink(), false);
      assert.equal(info.uid, process.getuid());
      assert.equal(info.mode & 0o777, 0o700, `${name} must be private before any child launch`);
    }
    assert.equal(createNavigationReader(evidence, "layout-329")().sequence, 0);
    assert.equal(process.umask(), 0o022);
  } finally {
    process.umask(mask);
    rmSync(evidence, { recursive: true });
  }
});

test("shared runner refuses an existing permissive webdriver directory without repairing it", () => {
  const mask = process.umask(0o022);
  const evidence = mkdtempSync(join(tmpdir(), "ca-run-layout-"));
  const logs = join(evidence, "webdriver");
  try {
    mkdirSync(logs, { mode: 0o755 });
    const before = lstatSync(logs);
    assert.equal(before.mode & 0o777, 0o755);
    assert.throws(() => createRunDirectories(evidence), { code: "EEXIST" });
    const after = lstatSync(logs);
    assert.equal(after.ino, before.ino);
    assert.equal(after.mode, before.mode);
    assert.throws(() => createNavigationReader(evidence, "layout-329")(),
      /private owned native log directory required/);
    assert.equal(process.umask(), 0o022);
  } finally {
    process.umask(mask);
    rmSync(evidence, { recursive: true });
  }
});
