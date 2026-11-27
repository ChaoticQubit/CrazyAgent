import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { ALLOWLIST, check, run, scan } from "./engine-guard.mjs";

const script = fileURLToPath(new URL("./engine-guard.mjs", import.meta.url));
const FILE = "crates/a/src/launch.rs";
const ADVICE = `guard it against the engine root or name its guard in ${ALLOWLIST}`;

/**
 * A Rust function that builds a project sandbox and installs it, guarded or not.
 * Inputs: the function's name and whether it guards. Output: the function's text.
 */
function builder(name, guarded) {
  const guard = guarded ? "        .guard_engine(engine)?\n" : "";
  return (
    `pub fn ${name}(root: &Path, engine: &Path, profile: &Path) -> Result<(), Error> {\n` +
    "    let sandbox = ProjectSandbox::new(&HOOKS, root, root, root, network())?\n" +
    `${guard}        .allow_read(root)?;\n` +
    "    sandbox.install(profile)?;\n" +
    "    Ok(())\n" +
    "}\n"
  );
}

/**
 * A throwaway git repository holding the given files, all tracked, and the allowlist.
 * Inputs: a map of relative path to text, and the allowlist. Output: the repository folder.
 */
function repository(files, allow) {
  const root = mkdtempSync(path.join(tmpdir(), "engine-guard-"));
  execFileSync("git", ["init", "-q"], { cwd: root });
  for (const [name, text] of Object.entries({ ...files, [ALLOWLIST]: JSON.stringify(allow) })) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, text);
  }
  execFileSync("git", ["add", "-A"], { cwd: root });
  return root;
}

test("an unguarded sandbox install fails with its line; a guarded one passes", () => {
  const bare = scan(FILE, builder("launch", false));
  assert.deepEqual(bare.sites, [{ line: 4, fn: "launch", guarded: false }]);
  assert.deepEqual(check(new Map([[FILE, bare]]), {}), [
    `${FILE}:4: fn launch installs a sandbox with no guard_engine; ${ADVICE}`,
  ]);
  const guarded = scan(FILE, builder("launch", true));
  assert.deepEqual(guarded.sites, [{ line: 5, fn: "launch", guarded: true }]);
  assert.deepEqual(check(new Map([[FILE, guarded]]), {}), []);
});

test("a guard_engine_around guard counts, a commented-out guard does not", () => {
  const guarded = builder("launch", true);
  const around = guarded.replace("guard_engine(engine)", "guard_engine_around(e, r)");
  assert.equal(scan(FILE, around).sites[0].guarded, true);
  const commented = guarded.replace(".guard_engine(engine)?", "// .guard_engine(e)?");
  assert.equal(scan(FILE, commented).sites[0].guarded, false);
});

test("test code and files naming no sandbox type are not read", () => {
  const planted = builder("planted", false).replaceAll("\n", "\n    ");
  const inline = `#[cfg(test)]\nmod tests {\n    ${planted}}\n`;
  assert.deepEqual(scan(FILE, `${builder("launch", true)}${inline}`).sites.length, 1);
  assert.deepEqual(scan("crates/a/src/tests.rs", builder("planted", false)).sites, []);
  assert.deepEqual(scan(FILE, builder("planted", false), true).sites, []);
  const installer = "pub fn run(i: &Installer) {\n    i.install(dep, &pin);\n}\n";
  assert.deepEqual(scan(FILE, installer), { sites: [], guarded: new Map() });
});

test("a type's own install call is a site, in the innermost function holding it", () => {
  const text =
    "use kernel_sandbox::profile::ProjectSandbox;\n" +
    "pub fn outer() {\n" +
    "    fn inner(s: ProjectSandbox) {\n" +
    "        InstalledSandbox::install(s, &p, calls, strict);\n" +
    "    }\n" +
    "    WindowsSandbox::install(policy, helper, profile, grants);\n" +
    "}\n";
  assert.deepEqual(
    scan(FILE, text).sites.map(({ line, fn }) => [line, fn]),
    [
      [4, "inner"],
      [6, "outer"],
    ],
  );
});

test("an allowlist entry needs a guarding function or a reason, and goes once guarded", () => {
  const guard = "crates/a/src/guard.rs";
  const found = new Map([
    [FILE, scan(FILE, builder("launch", false))],
    [guard, scan(guard, builder("guarding", true) + builder("bare", false).replace("ins", "x"))],
  ]);
  const key = `${FILE}#launch`;
  assert.deepEqual(check(found, { [key]: [`${guard}#guarding`] }), []);
  assert.deepEqual(check(found, { [key]: ["installs what its caller built"] }), []);
  assert.deepEqual(check(found, { [key]: [`${guard}#bare`] }), [
    `${key}: guard ${guard}#bare holds no guard_engine`,
  ]);
  assert.deepEqual(check(found, { [key]: [`${guard}#gone`] }), [
    `${key}: guard ${guard}#gone names no function`,
  ]);
  assert.deepEqual(check(found, { [key]: [] }), [`${key}: entry [] is not a list of guards`]);
  assert.deepEqual(check(found, { [key]: [" "] }), [
    `${key}: guard " " is not a reason or a function`,
  ]);
  const fixed = new Map([[FILE, scan(FILE, builder("launch", true))]]);
  assert.deepEqual(check(fixed, { [key]: ["a reason"] }), [
    `${key}: holds no unguarded install; remove its entry`,
  ]);
});

test("a guard name shared by two functions guards only when both do", () => {
  const guard = "crates/a/src/guard.rs";
  const twice = builder("prepare", true) + builder("prepare", false);
  const found = new Map([
    [FILE, scan(FILE, builder("launch", false))],
    [guard, scan(guard, twice)],
  ]);
  assert.deepEqual(check(found, { [`${FILE}#launch`]: [`${guard}#prepare`] }), [
    `${guard}:11: fn prepare installs a sandbox with no guard_engine; ${ADVICE}`,
    `${FILE}#launch: guard ${guard}#prepare holds no guard_engine`,
  ]);
});

test("the check reads every tracked file of a repository and fails on a planted builder", () => {
  const root = repository({ [FILE]: builder("launch", true) }, {});
  try {
    assert.deepEqual(run(root), []);
    writeFileSync(path.join(root, FILE), builder("launch", true) + builder("planted", false));
    execFileSync("git", ["add", "-A"], { cwd: root });
    assert.deepEqual(run(root), [
      `${FILE}:11: fn planted installs a sandbox with no guard_engine; ${ADVICE}`,
    ]);
    writeFileSync(path.join(root, "crates/a/src/untracked.rs"), builder("loose", false));
    assert.equal(run(root).length, 1);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a module declared only for tests is test code with its folder", () => {
  const files = {
    "crates/a/src/lib.rs": '#[cfg(all(test, windows))]\nmod engine_rows;\n',
    "crates/a/src/engine_rows.rs": builder("planted", false),
  };
  const root = repository(files, {});
  try {
    assert.deepEqual(run(root), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the command exits 1 with the problems on a planted builder and 0 on a clean tree", () => {
  const root = repository({ [FILE]: builder("launch", false) }, {});
  try {
    const failed = spawnSync(process.execPath, [script, root], { encoding: "utf8" });
    assert.equal(failed.status, 1);
    assert.match(failed.stderr, /fn launch installs a sandbox with no guard_engine/);
    assert.match(failed.stderr, /1 unguarded sandbox install\(s\)/);
    writeFileSync(path.join(root, FILE), builder("launch", true));
    const passed = spawnSync(process.execPath, [script, root], { encoding: "utf8" });
    assert.equal(passed.status, 0, passed.stderr);
    assert.equal(passed.stdout, "every sandbox install is guarded against the engine root\n");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
