import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const REPO = fileURLToPath(new URL("../..", import.meta.url));
const SCRIPT = join(REPO, "scripts/ci/farm-rust-shard.sh");

/**
 * Runs farm-rust-shard.sh with a fake cargo that records its argv and the native-auth host.
 * Inputs: the script arguments and extra environment. Output: { status, stderr, args, host }.
 */
function shard(args, extra = {}) {
  const root = mkdtempSync(join(tmpdir(), "farm-shard-"));
  try {
    const bin = join(root, "bin");
    mkdirSync(bin);
    const record = join(root, "record");
    writeFileSync(join(bin, "cargo"), `printf '%s\\n' "$@" > "${record}"\nprintf '%s\\n' "$CRAZYAGENT_TEST_NATIVE_AUTH_HOST" >> "${record}"\n`);
    chmodSync(join(bin, "cargo"), 0o755);
    const archive = join(root, "farm-linux.tar.zst");
    writeFileSync(archive, "");
    const env = {
      PATH: `${bin}:${dirname(process.execPath)}:/usr/bin:/bin`,
      GITHUB_WORKSPACE: "/work/repo",
      NEXTEST_PROFILE: "farm",
      FARM_ENGINE_EXCLUDE: "agents_live",
      FARM_DESKTOP_EXCLUDE: "",
      ...extra,
    };
    const run = spawnSync("bash", [SCRIPT, ...args.map((arg) => arg.replace("ARCHIVE", archive))], { cwd: REPO, env, encoding: "utf8" });
    let lines = [];
    try {
      lines = readFileSync(record, "utf8").trimEnd().split("\n");
    } catch {
      lines = [];
    }
    return { status: run.status, stderr: run.stderr + run.stdout, args: lines.slice(0, -1), host: lines.at(-1) };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("a shard extracts the archive over the checkout, runs its slice under the farm filter and names the archived engine as the native-auth host", () => {
  const run = shard(["ARCHIVE", "2", "4"]);
  assert.equal(run.status, 0, run.stderr);
  assert.deepEqual(run.args.slice(0, 2), ["nextest", "run"]);
  const flag = (name) => run.args[run.args.indexOf(name) + 1];
  assert.equal(flag("--workspace-remap"), "/work/repo");
  assert.equal(flag("--extract-to"), "/work/repo");
  assert.ok(run.args.includes("--extract-overwrite"));
  assert.equal(flag("--partition"), "slice:2/4");
  assert.equal(flag("-E"), "not (binary_id(=crazyagent::agents_live))");
  assert.equal(run.host, "/work/repo/target/debug/crazyagent");
});

test("a shard refuses a bad k/N, a missing archive or another nextest profile before running anything", () => {
  for (const [args, extra, message] of [
    [["ARCHIVE", "5", "4"], {}, /no k\/N/],
    [["ARCHIVE", "0", "4"], {}, /no k\/N/],
    [["ARCHIVE", "x", "4"], {}, /no k\/N/],
    [["/nowhere/farm.tar.zst", "1", "4"], {}, /no test archive/],
    [["ARCHIVE", "1", "4"], { NEXTEST_PROFILE: "default" }, /NEXTEST_PROFILE must be farm/],
    [["ARCHIVE", "1", "4"], { FARM_ENGINE_EXCLUDE: "a/b/c" }, /exclusion/],
  ]) {
    const run = shard(args, extra);
    assert.notEqual(run.status, 0, `${args} ${JSON.stringify(extra)}`);
    assert.match(run.stderr, message);
    assert.deepEqual(run.args, [], "cargo never ran");
  }
});
