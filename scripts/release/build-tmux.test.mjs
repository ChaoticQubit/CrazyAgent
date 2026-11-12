import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const SCRIPT = fileURLToPath(new URL("./build-tmux.sh", import.meta.url));
const SOURCES = ["TMUX", "LIBEVENT", "UTF8PROC", "NCURSES", "JEMALLOC"];

/**
 * Reads one pin the build script sets on a line of its own, such as `TMUX_VERSION=3.8`.
 * Inputs: the script text and the variable name. Output: the value without quotes.
 */
function pin(text, name) {
  const match = text.match(new RegExp(`^${name}="?([^"\\s]+)"?$`, "m"));
  assert(match, `${name} is pinned on a line of its own`);
  return match[1];
}

/**
 * Runs the build script with arguments that must be refused before any download, under a
 * PATH with no curl, so a refusal that came after a fetch would fail differently.
 * Input: the arguments. Output: the exit status and stderr.
 */
function refused(args) {
  const run = spawnSync("/bin/bash", [SCRIPT, ...args], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" }, timeout: 10000 });
  return { status: run.status, stderr: run.stderr };
}

test("the bundled tmux is the latest stable tmux 3.8 and every source tarball is pinned by its https URL and SHA-256 (DEC-157)", () => {
  const text = readFileSync(SCRIPT, "utf8");
  assert.equal(pin(text, "TMUX_VERSION"), "3.8");
  assert.equal(pin(text, "TMUX_SHA256"), "e79c699c7e949dccd0a4a125e17b8d1261e311b16979dbc8eb34542f3966d82e");
  assert.deepEqual(["LIBEVENT", "UTF8PROC", "NCURSES", "JEMALLOC"].map((name) => pin(text, `${name}_VERSION`)), ["2.1.13-stable", "2.12.0", "6.6", "5.4.0"]);
  const hashes = SOURCES.map((name) => pin(text, `${name}_SHA256`));
  for (const [index, name] of SOURCES.entries()) {
    const url = pin(text, `${name}_URL`);
    assert.match(url, /^https:\/\/[a-z0-9.-]+\/\S+\.tar\.(gz|bz2)$/, `${name} is one tarball over https`);
    assert(url.includes(pin(text, `${name}_VERSION`)), `${name}'s URL names its pinned version`);
    assert.match(hashes[index], /^[0-9a-f]{64}$/, `${name} is pinned by SHA-256`);
  }
  assert.equal(new Set(hashes).size, SOURCES.length, "each tarball has its own hash");
  assert.deepEqual([pin(text, "MACOS_MIN_AARCH64"), pin(text, "MACOS_MIN_X86_64")], ["11.0", "10.13"], "the app's minimum macOS");
});

test("the tmux build follows only https, installs nothing on the machine and carries no comments", () => {
  const text = readFileSync(SCRIPT, "utf8");
  assert.match(text, /^#!\/usr\/bin\/env bash\nset -euo pipefail\n/);
  assert(text.includes("--proto '=https'") && text.includes("--proto-redir '=https'"), "every fetch and redirect stays on https");
  assert(!/\b(brew|sudo|apt-get|dnf|yum)\b/.test(text), "no package manager and no root");
  assert.deepEqual(text.split("\n").slice(1).filter((line) => /^\s*#/.test(line)), [], "no comment lines");
  assert((statSync(SCRIPT).mode & 0o111) !== 0, "the release job runs it directly");
});

test("on macOS tmux links jemalloc as Homebrew's tmux does, force-loaded so its malloc zone replaces the calloc tmux refuses to trust, with each architecture's page size", () => {
  const text = readFileSync(SCRIPT, "utf8");
  assert.match(text, /JEMALLOC_LIBS="-Wl,-force_load,\$prefix\/lib\/libjemalloc\.a"/);
  assert.match(text, /--enable-jemalloc/);
  assert.match(text, /aarch64-apple-darwin\) [^\n]*page=14/);
  assert.match(text, /x86_64-apple-darwin\) [^\n]*page=12/);
  assert.match(text, /--with-lg-page="\$page"/);
  assert.match(text, /check_jemalloc/, "the built tmux is checked to hold jemalloc's zone");
});

test("the tmux build refuses an unknown target, a used work folder or an existing output before it downloads anything", () => {
  const root = mkdtempSync(join(tmpdir(), "build-tmux-"));
  try {
    const out = join(root, "crazyagent-tmux");
    assert.equal(refused([]).status, 2);
    assert.match(refused([]).stderr, /usage: build-tmux\.sh --target/);
    const target = refused(["--target", "sparc-sun-solaris", "--work", join(root, "work"), "--out", out]);
    assert.equal(target.status, 2);
    assert.match(target.stderr, /build_tmux_target_rejected/);
    const work = refused(["--target", "aarch64-apple-darwin", "--work", root, "--out", out]);
    assert.equal(work.status, 2);
    assert.match(work.stderr, /build_tmux_work_exists/);
    writeFileSync(out, "");
    const kept = refused(["--target", "aarch64-apple-darwin", "--work", join(root, "work"), "--out", out]);
    assert.equal(kept.status, 2);
    assert.match(kept.stderr, /build_tmux_out_exists/);
    assert.equal(readFileSync(out, "utf8"), "", "an existing output is never overwritten");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
