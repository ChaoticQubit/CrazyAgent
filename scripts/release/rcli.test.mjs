import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, symlink } from "node:fs/promises";
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
  assert.deepEqual(compile, ["/b/bun", "build", "/w/bundle/entry.js", "--compile", "--compile-executable-path=/b/bun", "--target=bun", "--packages=bundle", "--reject-unresolved", "--env=disable", "--outfile=/o/crazyagent-rcli"]);
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
