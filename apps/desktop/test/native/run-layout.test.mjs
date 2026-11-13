import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { chooseHome, clearWebStore, createRunDirectories, refusedGraphifyPaths, refusedToolHome, runProjects } from "./run-layout.mjs";
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

test("runs that share one prepared CRAZYAGENT_E2E_HOME each get a new projects folder and start with no web store an earlier run left, keeping the keychain", () => {
  withHomes((paths) => {
    const keychain = join(paths.prepared, "Library", "Keychains", "login.keychain-db");
    mkdirSync(dirname(keychain), { recursive: true, mode: 0o700 });
    writeFileSync(keychain, "kept");
    clearWebStore(paths.prepared);
    const first = runProjects(paths.prepared);
    const stored = join(paths.prepared, "Library", "WebKit", "crazyagent-desktop", "WebsiteData", "localstorage.sqlite3");
    mkdirSync(dirname(stored), { recursive: true, mode: 0o700 });
    writeFileSync(stored, "an earlier run's saved key");
    writeFileSync(join(first, "earlier-project"), "an earlier run's project");
    clearWebStore(paths.prepared);
    const second = runProjects(paths.prepared);
    assert.notEqual(second, first, "the second run on the same home does not reuse the first run's projects");
    assert.equal(dirname(second), paths.prepared);
    assert.deepEqual(readdirSync(second), []);
    assert.equal(lstatSync(second).mode & 0o777, 0o700);
    assert.equal(existsSync(join(paths.prepared, "Library", "WebKit")), false, "no web store an earlier run left");
    assert.equal(readFileSync(keychain, "utf8"), "kept", "the prepared keychain stays");
    clearWebStore(paths.fresh);
    assert.equal(dirname(runProjects(paths.fresh)), paths.fresh);
  });
  const runner = source("./run.mjs");
  assert.match(runner, /clearWebStore\(home\);/);
  assert.match(runner, /projects: runProjects\(home\)/);
  assert.doesNotMatch(runner, /join\(env\.HOME, "Projects"\)/);
  const conf = source("./master-switch.wdio.conf.mjs");
  assert.match(conf, /clearWebStore\(home\);/);
  assert.match(conf, /CODEX_HOME: join\(root, "codex"\)/, "the Codex home is the run's own too");
  assert.doesNotMatch(conf, /join\(home, "\.codex"\)/);
});

test("clearing a prepared home's web store never reaches through a linked Library, and a linked WebKit loses only its link", () => {
  withHomes((paths) => {
    const elsewhere = join(paths.real, "Library", "WebKit", "kept");
    mkdirSync(dirname(elsewhere), { recursive: true, mode: 0o700 });
    writeFileSync(elsewhere, "the real web store");
    symlinkSync(join(paths.real, "Library"), join(paths.prepared, "Library"));
    assert.throws(() => clearWebStore(paths.prepared), /refusing to clear .*Library: not a plain folder/);
    assert.equal(readFileSync(elsewhere, "utf8"), "the real web store", "the folder the link names keeps its web store");
    rmSync(join(paths.prepared, "Library"));
    writeFileSync(join(paths.prepared, "Library"), "");
    assert.throws(() => clearWebStore(paths.prepared), /refusing to clear .*Library: not a plain folder/);
    rmSync(join(paths.prepared, "Library"));
    mkdirSync(join(paths.prepared, "Library"), { mode: 0o700 });
    symlinkSync(join(paths.real, "Library", "WebKit"), join(paths.prepared, "Library", "WebKit"));
    clearWebStore(paths.prepared);
    assert.equal(existsSync(join(paths.prepared, "Library", "WebKit")), false);
    assert.equal(readFileSync(elsewhere, "utf8"), "the real web store", "a linked WebKit loses only its link");
    rmSync(join(paths.prepared, "Library"), { recursive: true });
    clearWebStore(paths.prepared);
  });
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

test("the refused-tool suite's home lies outside the checkout, holds a uv graphify whose installation the group can change and whose link leads out to a 0777 file, and is never a prepared home", () => {
  const mask = process.umask(0o022);
  const parent = mkdtempSync(join(tmpdir(), "ca-refused-"));
  try {
    assert.throws(() => refusedToolHome({ CRAZYAGENT_E2E_HOME: parent }, parent), /needs a fresh one: unset CRAZYAGENT_E2E_HOME/);
    assert.deepEqual(readdirSync(parent), [], "a refused prepared home plants nothing");
    const { home, prepared } = refusedToolHome({}, parent);
    assert.equal(prepared, false);
    assert.equal(home, realpathSync(home), "the home is real, so the fix tree lies strictly below it as the engine sees it");
    assert.equal(dirname(home), realpathSync(parent));
    const { prefix, outside } = refusedGraphifyPaths(home);
    const tool = join(home, ".local/share/uv/tools/graphifyy/bin");
    for (const folder of [prefix, join(prefix, "bin")]) assert.equal(statSync(folder).mode & 0o777, 0o775, folder);
    assert.equal(statSync(outside).mode & 0o777, 0o777);
    assert.equal(lstatSync(join(prefix, "outside")).isSymbolicLink(), true);
    assert.equal(realpathSync(join(prefix, "outside")), outside);
    assert.equal(realpathSync(join(home, ".local/bin/graphify")), join(tool, "graphify"));
    assert.equal(realpathSync(join(tool, "python")), realpathSync("/bin/bash"));
    assert.equal(statSync(join(tool, "graphify")).mode & 0o777, 0o755);
    assert.equal(readFileSync(join(tool, "graphify"), "utf8"), `#!${join(tool, "python")}\necho 'graphify 0.9.9'\n`);
    const runner = source("./run.mjs");
    assert.match(runner, /suite === "refused-tool" \? refusedToolHome\(\) : chooseHome\(join\(evidence, "home"\)\)/);
    assert.match(source("./model-access.wdio.conf.mjs"), /"refused-tool": "refused-tool\.e2e\.mjs"/);
  } finally {
    process.umask(mask);
    rmSync(parent, { recursive: true });
  }
});
