import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { ALLOW_MAX, ALLOWLIST, check, run, scan } from "./engine-guard.mjs";

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

/**
 * An allowlist entry for a function with one unguarded install. Input: its guards.
 * Output: the entry.
 */
function pin(guards) {
  return { sites: 1, guards };
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

test("a guard after the install call in its function does not cover it", () => {
  const late =
    "pub fn launch(root: &Path, engine: &Path, profile: &Path) -> Result<(), Error> {\n" +
    "    let sandbox = ProjectSandbox::new(&HOOKS, root, root, root, network())?;\n" +
    "    sandbox.install(profile)?;\n" +
    "    let later = sandbox.guard_engine(engine)?;\n" +
    "    later.install(profile)?;\n" +
    "    sandbox.guard_engine(engine)?.install(profile)?;\n" +
    "    sandbox.install(profile).and(sandbox.guard_engine(engine))?;\n" +
    "    Ok(())\n" +
    "}\n";
  assert.deepEqual(
    scan(FILE, late).sites.map(({ line, guarded }) => [line, guarded]),
    [
      [3, false],
      [5, true],
      [6, true],
      [7, false],
    ],
  );
  const kept = late.split("\n").filter((_, at) => at < 4 || at > 6);
  const alone = scan(FILE, kept.join("\n"));
  assert.deepEqual(check(new Map([[FILE, alone]]), {}), [
    `${FILE}:3: fn launch installs a sandbox with no guard_engine; ${ADVICE}`,
  ]);
  const same = kept.map((line, at) => (at === 3 ? late.split("\n")[6] : line));
  assert.deepEqual(scan(FILE, same.join("\n")).sites[1], { line: 4, fn: "launch", guarded: false });
});

/**
 * A function that makes a sandbox, then runs the given lines, then installs `sandbox`.
 * Input: the lines between, each without its indent. Output: the function's text.
 */
function installing(lines) {
  return (
    "pub fn launch(state: &Path, engine: &Path, profile: &Path) -> Result<(), Error> {\n" +
    "    let mut sandbox = ProjectSandbox::new(&HOOKS, root, root, root, network())?;\n" +
    lines.map((line) => `    ${line}\n`).join("") +
    "    sandbox.install(profile)?;\n" +
    "    Ok(())\n" +
    "}\n"
  );
}

/** Whether the last install site of a function's text is guarded. Input: the text. */
function lastGuarded(text) {
  const { sites } = scan(FILE, text);
  assert.ok(sites.length > 0, text);
  return sites.at(-1).guarded;
}

test("a guard counts only on the install's receiver, never on another sandbox", () => {
  assert.equal(lastGuarded(installing(["sandbox = sandbox.guard_engine(engine)?;"])), true);
  assert.equal(lastGuarded(installing(["let other = sandbox.guard_engine(engine)?;"])), false);
  const other = ["let guarded = ProjectSandbox::new(&HOOKS, a, a, a, n)?.guard_engine(e)?;"];
  assert.equal(lastGuarded(installing(other)), false);
  const remade = [
    "sandbox = sandbox.guard_engine(engine)?;",
    "let sandbox = ProjectSandbox::new(&HOOKS, root, root, root, network())?;",
  ];
  assert.equal(lastGuarded(installing(remade)), false);
  const derived = [
    "sandbox = sandbox.guard_engine(engine)?;",
    "(sandbox, _) = sandbox.allow_tools(&t)?;",
  ];
  assert.equal(lastGuarded(installing(derived)), true);
});

test("a guard in an if branch does not cover an install; the same guard unconditional does", () => {
  const branch = [
    "if let Some(runtime) = state.parent() {",
    "    sandbox = sandbox.guard_engine(runtime)?;",
    "}",
  ];
  assert.equal(lastGuarded(installing(branch)), false);
  const matched = [
    "match state.parent() {",
    "    Some(runtime) => sandbox = sandbox.guard_engine(runtime)?,",
    "    None => {}",
    "}",
  ];
  assert.equal(lastGuarded(installing(matched)), false);
  const plain = [
    "let runtime = state.parent().ok_or(Error::Unparented)?;",
    "sandbox = sandbox.guard_engine(runtime)?;",
  ];
  assert.equal(lastGuarded(installing(plain)), true);
});

test("a guard in a closure block does not cover an install; a bare closure step does", () => {
  const block = ["sandbox = Ok(sandbox).and_then(|s| { s.guard_engine(engine) })?;"];
  assert.equal(lastGuarded(installing(block)), false);
  const kept = ["let guard = |s: ProjectSandbox| s.guard_engine(engine);"];
  assert.equal(lastGuarded(installing(kept)), false);
  const step = ["sandbox = Ok(sandbox).and_then(|s| s.guard_engine(engine))?;"];
  assert.equal(lastGuarded(installing(step)), true);
});

test("an install on a guarded chain, a closure argument or a split argument is guarded", () => {
  const chained =
    "pub fn launch(engine: &Path, profile: &Path) -> Result<InstalledSandbox, Error> {\n" +
    "    ProjectSandbox::new(&HOOKS, root, root, root, network())\n" +
    "        .and_then(|sandbox| sandbox.guard_engine(engine))\n" +
    "        .and_then(|sandbox| InstalledSandbox::install(sandbox, &profile, calls, strict))\n" +
    "}\n";
  assert.deepEqual(scan(FILE, chained).sites, [{ line: 4, fn: "launch", guarded: true }]);
  const bare = chained.replace("        .and_then(|sandbox| sandbox.guard_engine(engine))\n", "");
  assert.deepEqual(scan(FILE, bare).sites, [{ line: 3, fn: "launch", guarded: false }]);
  const split = installing(["sandbox = sandbox.guard_engine(engine)?;"]).replace(
    "    sandbox.install(profile)?;\n",
    "    let boundary = InstalledSandbox::install(\n        sandbox,\n        profile,\n    )?;\n",
  );
  assert.equal(lastGuarded(split), true);
  assert.equal(lastGuarded(split.replace("sandbox = sandbox.guard_engine(engine)?;", "")), false);
});

test("a guard written in a string or a block comment does not count", () => {
  const quoted = ['let note = "sandbox = sandbox.guard_engine(engine)?;";'];
  assert.equal(lastGuarded(installing(quoted)), false);
  const commented = ["/* sandbox = sandbox.guard_engine(engine)?; */"];
  assert.equal(lastGuarded(installing(commented)), false);
});

test("a builder must guard a sandbox it made; one passing a sandbox on need not", () => {
  const made =
    "pub fn policy(engine: &Path) -> Result<ProjectSandbox, SandboxError> {\n" +
    "    ProjectSandbox::new(&HOOKS, root, root, root, network())?\n" +
    "        .guard_engine(engine)?\n" +
    "        .allow_read(root)\n" +
    "}\n";
  assert.deepEqual(scan(FILE, made).sites, []);
  const bare = made.replace("        .guard_engine(engine)?\n", "");
  assert.deepEqual(scan(FILE, bare).sites, [
    { line: 1, fn: "policy", guarded: false, returns: true },
  ]);
  assert.deepEqual(check(new Map([[FILE, scan(FILE, bare)]]), {}), [
    `${FILE}:1: fn policy returns a sandbox it built with no guard_engine; ${ADVICE}`,
  ]);
  const pinned = { [`${FILE}#policy`]: pin(["a reason naming crates/a/src/launch.rs#policy"]) };
  assert.deepEqual(check(new Map([[FILE, scan(FILE, bare)]]), pinned), []);
  const tuple = bare.replace("Result<ProjectSandbox,", "Result<(ProjectSandbox, Hidden),");
  assert.equal(scan(FILE, tuple).sites.length, 1);
  const passing =
    "pub fn apply(sandbox: ProjectSandbox, root: &Path) -> Result<ProjectSandbox, Error> {\n" +
    "    sandbox.allow_read(root)\n" +
    "}\n";
  assert.deepEqual(scan(FILE, passing).sites, []);
  const maker = "    ProjectSandbox::new(&HOOKS, root, root, root, n())\n";
  const swapped = passing.replace("    sandbox.allow_read(root)\n", maker);
  assert.equal(scan(FILE, swapped).sites.length, 1);
  const borrowed = "pub fn sandbox(&self) -> &ProjectSandbox {\n    &self.sandbox\n}\n";
  assert.deepEqual(scan(FILE, borrowed).sites, []);
});

test("an allowlist past its most entries fails, at the most it passes", () => {
  const found = new Map([[FILE, scan(FILE, builder("launch", true))]]);
  const reasons = (count) =>
    Object.fromEntries(
      Array.from({ length: count }, (_, at) => [`${FILE}#gone${at}`, pin([`${FILE}#launch`])]),
    );
  const at = check(found, reasons(ALLOW_MAX));
  assert.ok(at.every((line) => line.endsWith("holds no unguarded install; remove its entry")));
  assert.equal(at.length, ALLOW_MAX);
  const over = check(found, reasons(ALLOW_MAX + 1));
  const refused = `${ALLOW_MAX + 1} entries, at most ${ALLOW_MAX}; the allowlist only shrinks`;
  assert.ok(over.includes(`${ALLOWLIST}: ${refused}`), over.join("\n"));
});

test("test code and files naming no sandbox type are not read", () => {
  const planted = builder("planted", false).replaceAll("\n", "\n    ");
  const inline = `#[cfg(test)]\nmod tests {\n    ${planted}}\n`;
  assert.deepEqual(scan(FILE, `${builder("launch", true)}${inline}`).sites.length, 1);
  assert.deepEqual(scan("crates/a/src/tests.rs", builder("planted", false)).sites, []);
  assert.deepEqual(scan(FILE, builder("planted", false), true).sites, []);
  const installer = "pub fn run(i: &Installer) {\n    i.install(dep, &pin);\n}\n";
  assert.deepEqual(scan(FILE, installer), { sites: [], guarded: new Map([["run", false]]) });
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
  assert.deepEqual(check(found, { [key]: pin([`${guard}#guarding`]) }), []);
  assert.deepEqual(check(found, { [key]: pin([`installs what ${guard}#bare built`]) }), []);
  assert.deepEqual(check(found, { [key]: pin(["installs what its caller built"]) }), [
    `${key}: reason names no function that still exists (none)`,
  ]);
  assert.deepEqual(check(found, { [key]: pin([`installs what ${guard}#gone built`]) }), [
    `${key}: reason names no function that still exists (${guard}#gone)`,
  ]);
  assert.deepEqual(check(found, { [key]: pin([`${guard}#bare`]) }), [
    `${key}: guard ${guard}#bare holds no guard_engine`,
  ]);
  assert.deepEqual(check(found, { [key]: pin([`${guard}#gone`]) }), [
    `${key}: guard ${guard}#gone names no function`,
  ]);
  const empty = { sites: 1, guards: [] };
  assert.deepEqual(check(found, { [key]: empty }), [
    `${key}: entry ${JSON.stringify(empty)} is not { sites, guards }`,
  ]);
  assert.deepEqual(check(found, { [key]: [`${guard}#guarding`] }), [
    `${key}: entry ["${guard}#guarding"] is not { sites, guards }`,
  ]);
  assert.deepEqual(check(found, { [key]: pin([" "]) }), [
    `${key}: guard " " is not a reason or a function`,
  ]);
  const fixed = new Map([[FILE, scan(FILE, builder("launch", true))]]);
  assert.deepEqual(check(fixed, { [key]: pin([`${FILE}#launch`]) }), [
    `${key}: holds no unguarded install; remove its entry`,
  ]);
});

test("a new install in an allowlisted function changes its pinned count and fails", () => {
  const guard = "crates/a/src/context_mode.rs";
  const wrap = "crates/a/src/context_mode/unix.rs";
  const reason = "crates/a/src/runner.rs";
  const once = builder("npm_wrap", false);
  const twice = once.replace("    Ok(())\n", "    sandbox.install(other)?;\n    Ok(())\n");
  const tree = (wrapped, launched) =>
    new Map([
      [guard, scan(guard, builder("npm_command", true))],
      [wrap, scan(wrap, wrapped)],
      [reason, scan(reason, launched.replaceAll("npm_wrap", "launch_leased"))],
    ]);
  const allow = {
    [`${wrap}#npm_wrap`]: pin([`${guard}#npm_command`]),
    [`${reason}#launch_leased`]: pin([`installs a launch context: ${guard}#npm_command`]),
  };
  assert.deepEqual(check(tree(once, once), allow), []);
  assert.deepEqual(check(tree(twice, once), allow), [
    `${wrap}#npm_wrap: 2 unguarded installs (lines 4,5), entry pins 1`,
  ]);
  assert.deepEqual(check(tree(once, twice), allow), [
    `${reason}#launch_leased: 2 unguarded installs (lines 4,5), entry pins 1`,
  ]);
  const both = { ...allow, [`${wrap}#npm_wrap`]: { sites: 2, guards: [`${guard}#npm_command`] } };
  assert.deepEqual(check(tree(twice, once), both), []);
});

test("a guard name shared by two functions guards only when both do", () => {
  const guard = "crates/a/src/guard.rs";
  const twice = builder("prepare", true) + builder("prepare", false);
  const found = new Map([
    [FILE, scan(FILE, builder("launch", false))],
    [guard, scan(guard, twice)],
  ]);
  assert.deepEqual(check(found, { [`${FILE}#launch`]: pin([`${guard}#prepare`]) }), [
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
    assert.match(failed.stderr, /1 engine guard problem\(s\)/);
    writeFileSync(path.join(root, FILE), builder("launch", true));
    const passed = spawnSync(process.execPath, [script, root], { encoding: "utf8" });
    assert.equal(passed.status, 0, passed.stderr);
    assert.equal(passed.stdout, "every sandbox install is guarded against the engine root\n");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
