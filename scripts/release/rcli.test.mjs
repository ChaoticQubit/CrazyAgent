import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { buildRcli, rcliCommands } from "./rcli.mjs";

function required(name) {
  const value = process.env[name];
  assert(value, `${name} must name the prerequisite for this test (bun executable or pinned Backboard-R-CLI checkout with installed node_modules)`);
  return value;
}

test("the rcli host compiles in two bun stages with no environment, dotenv or install access", () => {
  const [bundle, compile] = rcliCommands({ bun: "/b/bun", entry: "/r/scripts/rcli/entry.ts", work: "/w", output: "/o/crazyagent-rcli" });
  assert.deepEqual(bundle, ["/b/bun", "build", "/r/scripts/rcli/entry.ts", "--target=bun", "--format=esm", "--packages=bundle", "--reject-unresolved", "--env=disable", "--outdir=/w/bundle", "--entry-naming=entry.js"]);
  assert.deepEqual(compile, ["/b/bun", "build", "/w/bundle/entry.js", "--compile", "--compile-executable-path=/b/bun", "--target=bun", "--packages=bundle", "--reject-unresolved", "--env=disable", "--no-compile-autoload-dotenv", "--no-compile-autoload-bunfig", "--no-compile-autoload-tsconfig", "--no-compile-autoload-package-json", "--outfile=/o/crazyagent-rcli"]);
});

test("an rcli source alias that points elsewhere is refused before bun runs", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-rcli-alias-"));
  try {
    await mkdir(join(root, ".local-state/tools"), { recursive: true });
    await mkdir(join(root, "elsewhere"));
    await symlink(join(root, "elsewhere"), join(root, ".local-state/tools/rcli-source"));
    await assert.rejects(buildRcli({ repo: root, sdk: required("CRAZYAGENT_RCLI_SDK"), bun: required("CRAZYAGENT_BUN"), output: join(root, "crazyagent-rcli") }), /release_rcli_sdk_alias_rejected/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("the built rcli host is a native executable that runs the CrazyAgent entry", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-rcli-"));
  try {
    const output = join(root, process.platform === "win32" ? "crazyagent-rcli.exe" : "crazyagent-rcli");
    const built = await buildRcli({ sdk: required("CRAZYAGENT_RCLI_SDK"), bun: required("CRAZYAGENT_BUN"), output });
    assert.equal(built.output, output);
    const bytes = await readFile(output);
    assert.equal(built.bytes, bytes.length);
    assert(bytes.length > 16 * 1024 * 1024);
    const magic = { darwin: (b) => b.readUInt32LE(0) === 0xfeedfacf, linux: (b) => b.readUInt32BE(0) === 0x7f454c46, win32: (b) => b.readUInt16LE(0) === 0x5a4d }[process.platform];
    assert(magic(bytes));
    const env = process.platform === "win32" ? { SystemRoot: process.env.SystemRoot, USERPROFILE: root, TEMP: root, TMP: root } : { PATH: "/usr/bin:/bin", HOME: root, TMPDIR: root };
    const result = spawnSync(output, [], { stdio: ["ignore", "pipe", "pipe"], env, timeout: 60_000 });
    assert.equal(result.status, 70);
    assert.equal(String(result.stderr), "host_execution_failed\n");
    assert.equal(String(result.stdout), "");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a compiled rcli host takes no bunfig preload or .env from the folder it runs in or its home", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-rcli-autoload-"));
  try {
    const bun = required("CRAZYAGENT_BUN");
    const entry = join(root, "entry.ts");
    await writeFile(entry, 'process.stdout.write("DUMMY=" + (process.env.DUMMY_VALUE ?? "none"));\n');
    const output = join(root, process.platform === "win32" ? "host.exe" : "host");
    for (const argv of rcliCommands({ bun, entry, work: join(root, "work"), output })) {
      const built = spawnSync(argv[0], argv.slice(1), { cwd: root, stdio: ["ignore", "pipe", "pipe"], timeout: 60_000 });
      assert.equal(built.status, 0, String(built.stderr));
    }
    const folder = join(root, "project");
    await mkdir(folder);
    await writeFile(join(folder, "bunfig.toml"), 'preload = ["./canary.ts"]\n');
    await writeFile(join(folder, "canary.ts"), 'await Bun.write(import.meta.dir + "/marker", "ran");\n');
    await writeFile(join(folder, ".env"), "DUMMY_VALUE=1\n");
    await writeFile(join(root, ".bunfig.toml"), `preload = [${JSON.stringify(join(root, "home-canary.ts"))}]\n`);
    await writeFile(join(root, "home-canary.ts"), 'await Bun.write(import.meta.dir + "/home-marker", "ran");\n');
    const env = process.platform === "win32" ? { SystemRoot: process.env.SystemRoot, USERPROFILE: root, TEMP: root, TMP: root } : { PATH: "/usr/bin:/bin", HOME: root, TMPDIR: root };
    const result = spawnSync(output, [], { cwd: folder, stdio: ["ignore", "pipe", "pipe"], env, timeout: 60_000 });
    assert.equal(result.status, 0, String(result.stderr));
    assert.equal(String(result.stdout), "DUMMY=none");
    assert.equal(existsSync(join(root, "home-marker")), false, "a bunfig in the home folder ran");
    assert.equal(existsSync(join(folder, "marker")), false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
