import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { applyVersion, channelVersion, workspaceVersion } from "./version.mjs";

const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const FILES = ["Cargo.toml", "Cargo.lock", "apps/desktop/package.json", "apps/desktop/package-lock.json", "apps/desktop/src-tauri/tauri.conf.json"];

async function exported() {
  const root = await mkdtemp(join(tmpdir(), "release-version-"));
  execFileSync("sh", ["-c", `git -C "$1" archive HEAD | tar -x -C "$2"`, "sh", REPO, root]);
  return root;
}

test("dev versions carry the next stable and a UTC minute stamp", () => {
  assert.equal(channelVersion({ channel: "dev", base: "0.2.0", now: new Date(Date.UTC(2026, 9, 25, 16, 7, 59)) }), "0.2.0-dev.202610251607");
  assert.equal(channelVersion({ channel: "dev", base: "1.10.3", now: new Date(Date.UTC(2027, 0, 2, 3, 4)) }), "1.10.3-dev.202701020304");
  assert.throws(() => channelVersion({ channel: "dev", base: "0.2.0-dev.1", now: new Date() }), /release_base_version_rejected/);
  assert.throws(() => channelVersion({ channel: "dev", base: "0.2", now: new Date() }), /release_base_version_rejected/);
});

test("stable versions come only from a plain vX.Y.Z tag", () => {
  assert.equal(channelVersion({ channel: "stable", tag: "v0.2.0" }), "0.2.0");
  assert.equal(channelVersion({ channel: "stable", tag: "refs/tags/v12.0.7" }), "12.0.7");
  for (const tag of ["0.2.0", "v0.2", "v0.2.0-rc.1", "v01.2.3", "v0.2.0\n", "dev", undefined]) {
    assert.throws(() => channelVersion({ channel: "stable", tag }), /release_tag_rejected/);
  }
  assert.throws(() => channelVersion({ channel: "nightly", tag: "v0.2.0" }), /release_channel_rejected/);
});

test("applying a version rewrites every manifest and lock entry and nothing else", async () => {
  const root = await exported();
  try {
    const before = Object.fromEntries(await Promise.all(FILES.map(async (file) => [file, await readFile(join(root, file), "utf8")])));
    const old = await workspaceVersion(root);
    const version = "0.9.4-dev.202610251607";
    const changed = await applyVersion(root, version);
    assert.deepEqual(changed.sort(), [...FILES].sort());
    assert.equal(await workspaceVersion(root), version);
    const expected = { "Cargo.toml": 1, "Cargo.lock": 3, "apps/desktop/package.json": 1, "apps/desktop/package-lock.json": 2, "apps/desktop/src-tauri/tauri.conf.json": 1 };
    for (const file of FILES) {
      const was = before[file].split("\n");
      const now = (await readFile(join(root, file), "utf8")).split("\n");
      assert.equal(now.length, was.length, file);
      const changedLines = now.map((line, index) => [was[index], line]).filter(([a, b]) => a !== b);
      assert.equal(changedLines.length, expected[file], file);
      for (const [a, b] of changedLines) assert.equal(b, a.replace(old, version), file);
    }
    const lock = await readFile(join(root, "Cargo.lock"), "utf8");
    for (const name of ["crazyagent", "crazyagent-desktop", "crazyagent-win-sandbox"]) {
      assert.match(lock, new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "${version.replaceAll(".", "\\.")}"\\n`));
    }
    const pkg = JSON.parse(await readFile(join(root, "apps/desktop/package-lock.json"), "utf8"));
    assert.equal(pkg.version, version);
    assert.equal(pkg.packages[""].version, version);
    assert.equal(JSON.parse(await readFile(join(root, "apps/desktop/package.json"), "utf8")).version, version);
    assert.equal(JSON.parse(await readFile(join(root, "apps/desktop/src-tauri/tauri.conf.json"), "utf8")).version, version);
    const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--format-version", "1", "--locked", "--offline", "--manifest-path", join(root, "Cargo.toml")], { maxBuffer: 64 * 1024 * 1024 }).toString());
    const members = metadata.packages.filter((item) => metadata.workspace_members.includes(item.id));
    assert.equal(members.length, 3);
    assert(members.every((item) => item.version === version));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a version that is not semver is refused before any file changes", async () => {
  const root = await exported();
  try {
    const before = await readFile(join(root, "Cargo.toml"), "utf8");
    for (const version of ["0.2", "v0.2.0", "0.2.0-", "0.2.0 ", "0.2.0-dev.01", "0.2.0\"x"]) {
      await assert.rejects(applyVersion(root, version), /release_version_rejected/);
    }
    assert.equal(await readFile(join(root, "Cargo.toml"), "utf8"), before);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
