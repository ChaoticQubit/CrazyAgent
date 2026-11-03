import assert from "node:assert/strict";
import { chmod, link, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { parseLaunch, readInstructions, runExecutable } from "./host.runtime.ts";

const argv = ["--session-id", "session", "--instance-id", "instance", "--home-dir", "/private/entry/home", "--cwd", "/private/entry/project"];

test("launch takes an instructions file only once, by name, outside the home and project folders", () => {
  const file = "/private/entry/runtime/instructions.md";
  const expected = { sessionId: "session", instanceId: "instance", homeDir: "/private/entry/home", cwd: "/private/entry/project", instructionsFile: file };
  assert.deepEqual(parseLaunch([...argv, "--instructions-file", file]), expected);
  assert.deepEqual(parseLaunch(["--instructions-file", file, ...argv]), expected);
  for (const args of [
    [...argv, "--instructions-file"],
    [...argv.slice(0, 6), "--instructions-file", file],
    [...argv.slice(0, 6), "--home-dir", "/private/entry/other", "--instructions-file", file],
    [...argv, "--session-id", "again"],
    [...argv, "--instructions-file", "/private/entry/home/instructions.md"],
    [...argv, "--instructions-file", "/private/entry/project/instructions.md"],
    [...argv, "--instructions-file", "/private/entry/project"],
    [...argv, "--instructions-file", "instructions.md"],
    [...argv, "--instructions-file", "/private/entry/runtime/../instructions.md"],
    [...argv, "--instructions-file", "/private/entry/runtime/\u0000instructions.md"],
  ]) {
    assert.throws(() => parseLaunch(args), /host_launch_rejected/, JSON.stringify(args));
  }
});

test("instructions are read only from a private single-link UTF-8 file of at most 64 KiB", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "rcli-instructions-")));
  try {
    await chmod(root, 0o700);
    const file = join(root, "instructions.md");
    await writeFile(file, "OWNER-RULE-86\n", { mode: 0o600 });
    assert.equal(await readInstructions(file), "OWNER-RULE-86\n");
    const linked = join(root, "linked.md");
    await symlink(file, linked);
    await assert.rejects(readInstructions(linked), /host_launch_rejected/);
    await chmod(file, 0o644);
    await assert.rejects(readInstructions(file), /host_launch_rejected/);
    await chmod(file, 0o600);
    const second = join(root, "second.md");
    await link(file, second);
    await assert.rejects(readInstructions(file), /host_launch_rejected/);
    await rm(second);
    assert.equal(await readInstructions(file), "OWNER-RULE-86\n");
    const whole = join(root, "whole.md");
    await writeFile(whole, Buffer.alloc(65_536, 0x61), { mode: 0o600 });
    assert.equal((await readInstructions(whole)).length, 65_536);
    for (const [name, bytes] of [
      ["empty.md", Buffer.alloc(0)],
      ["blank.md", Buffer.from(" \n\t\n")],
      ["nul.md", Buffer.from("a\u0000b")],
      ["latin.md", Buffer.from([0x61, 0xff, 0xfe])],
      ["large.md", Buffer.alloc(65_537, 0x61)],
    ]) {
      const path = join(root, name);
      await writeFile(path, bytes, { mode: 0o600 });
      await assert.rejects(readInstructions(path), /host_launch_rejected/, name);
    }
  } finally { await rm(root, { recursive: true }); }
});

test("the host refuses an unsafe instructions file before it loads the SDK", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "rcli-entry-flag-")));
  const home = process.env.HOME;
  try {
    await chmod(root, 0o700);
    const homeDir = join(root, "home");
    const cwd = join(root, "project");
    const runtime = join(root, "runtime");
    for (const folder of [homeDir, cwd, runtime]) await mkdir(folder, { mode: 0o700 });
    await mkdir(join(cwd, ".git"), { mode: 0o700 });
    const file = join(runtime, "instructions.md");
    await writeFile(file, "OWNER-RULE-86\n", { mode: 0o644 });
    const args = ["--session-id", "session", "--instance-id", "instance", "--home-dir", homeDir, "--cwd", cwd, "--instructions-file", file];
    process.env.HOME = homeDir;
    await assert.rejects(runExecutable(args), /^Error: host_launch_rejected$/);
  } finally {
    process.env.HOME = home;
    await rm(root, { recursive: true });
  }
});
