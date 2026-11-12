import assert from "node:assert/strict";
import test from "node:test";
import { lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chooseHome, createRunDirectories } from "./run-layout.mjs";
import { createNavigationReader } from "./refresh-barrier.mjs";

const source = (name) => readFileSync(new URL(name, import.meta.url), "utf8");

/** Runs one check against a scratch folder holding a fresh home, a prepared home, a file, a
 * stand-in real home and a link to it, then removes the folder. Input: the check, given the
 * folder's paths. Output: nothing. */
function withHomes(check) {
  const scratch = mkdtempSync(join(tmpdir(), "ca-e2e-home-"));
  try {
    const paths = { fresh: join(scratch, "fresh"), prepared: join(scratch, "prepared"), file: join(scratch, "file"),
      real: join(scratch, "real"), linked: join(scratch, "linked") };
    mkdirSync(paths.fresh, { mode: 0o700 });
    mkdirSync(paths.prepared, { mode: 0o700 });
    mkdirSync(paths.real, { mode: 0o700 });
    writeFileSync(paths.file, "");
    symlinkSync(paths.real, paths.linked);
    check(paths);
  } finally {
    rmSync(scratch, { recursive: true });
  }
}

/** Checks one runner's home choice for its fresh home: the fresh one by default, a prepared
 * folder when CRAZYAGENT_E2E_HOME names one, and a refusal for every other name. Inputs: the
 * paths. Output: nothing. */
function choosesOnlyAPreparedHome(paths) {
  const pick = (named) => chooseHome(paths.fresh, named === undefined ? {} : { CRAZYAGENT_E2E_HOME: named }, paths.real);
  assert.deepEqual(pick(undefined), { home: paths.fresh, prepared: false });
  assert.deepEqual(pick(""), { home: paths.fresh, prepared: false });
  assert.deepEqual(pick(paths.prepared), { home: paths.prepared, prepared: true });
  assert.throws(() => pick("prepared"), /CRAZYAGENT_E2E_HOME must be an absolute path/);
  assert.throws(() => pick(join(paths.prepared, "missing")), /CRAZYAGENT_E2E_HOME must name an existing folder/);
  assert.throws(() => pick(paths.file), /CRAZYAGENT_E2E_HOME must be a folder/);
  assert.throws(() => pick("/"), /CRAZYAGENT_E2E_HOME must be owned by this user/);
  assert.throws(() => pick(paths.real), /CRAZYAGENT_E2E_HOME must not be the real home/);
  assert.throws(() => pick(paths.linked), /CRAZYAGENT_E2E_HOME must not be the real home/);
}

test("run.mjs gives the app and engine a prepared CRAZYAGENT_E2E_HOME as HOME and CFFIXED_USER_HOME, else its fresh home", () => {
  withHomes(choosesOnlyAPreparedHome);
  const runner = source("./run.mjs");
  assert.match(runner, /chooseHome\(join\(evidence, "home"\)\)/);
  assert.match(runner, /HOME: home,/);
  assert.match(runner, /\.\.\.\(prepared \? \{ CFFIXED_USER_HOME: home \} : \{\}\)/, "the fresh home run keeps its old environment");
});

test("master-switch.wdio.conf gives the engine, the app and the spec's WebKit store a prepared CRAZYAGENT_E2E_HOME, else MS_ROOT/home", () => {
  withHomes(choosesOnlyAPreparedHome);
  const conf = source("./master-switch.wdio.conf.mjs");
  assert.match(conf, /export const home = chooseHome\(join\(root, "home"\)\)\.home;/);
  assert.match(conf, /HOME: home, CFFIXED_USER_HOME: home/);
  const spec = source("./master-switch.e2e.mjs");
  assert.match(spec, /import \{ TURN_S, home \} from "\.\/master-switch\.wdio\.conf\.mjs";/);
  assert.match(spec, /join\(home, "Library", "WebKit"/);
  assert.doesNotMatch(spec, /join\(root, "home"/);
});

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
