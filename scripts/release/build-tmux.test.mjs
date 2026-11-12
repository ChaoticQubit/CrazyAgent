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

test("on macOS no libc function newer than the app's minimum is linked weak: pipe2 (macOS 27) is turned off and any other weak import fails the build, since a weak symbol is NULL on older macOS and tmux crashed at start in libevent", () => {
  const text = readFileSync(SCRIPT, "utf8");
  assert.match(text, /^\s*configure_args=\("--host=\$\{target\}" [^\n]*\bac_cv_func_pipe2=no\b[^\n]*\)$/m, "every macOS configure is told pipe2 is missing");
  for (const name of ["build_jemalloc", "build_libevent", "build_tmux"]) {
    const body = text.slice(text.indexOf(`${name}() {`), text.indexOf("\n}\n", text.indexOf(`${name}() {`)));
    assert(body.includes('${configure_args[@]+"${configure_args[@]}"}'), `${name} passes the configure arguments`);
  }
  assert.equal(text.match(/^MACOS_WEAK_ALLOWED="([^"]*)"$/m)?.[1], "_malloc_default_purgeable_zone ___darwin_check_fd_set_overflow", "only the two weak imports whose callers check for NULL");
  assert.match(text, /^check_weak_imports\(\) \{\n[^]*?nm -m "\$binary"[^]*?\(undefined\\\) weak external[^]*?build_tmux_weak_import[^]*?\n\}$/m);
  assert.match(text, /^check_macos\(\) \{\n[^]*?check_weak_imports "\$binary" "\$minimum"[^]*?\n\}$/m);
  const macos = text.slice(text.indexOf('if [ "$os" = macos ]; then\n  export CC=clang'), text.indexOf("build_jemalloc \"$tree\""));
  assert.match(macos, /^ {2}export MACOSX_DEPLOYMENT_TARGET="\$minimum"$/m, "every dependency is built for the app's minimum macOS, not the SDK's");
});

test("every check reads its tool's whole output before it matches, so pipefail never fails a good build or passes a bad one when grep -q stops reading early", () => {
  const lines = readFileSync(SCRIPT, "utf8").split("\n");
  assert.deepEqual(lines.filter((line) => /\|\s*grep\b/.test(line)), [], "no tool output is piped into grep");
  assert.deepEqual(lines.filter((line) => /\|\| true/.test(line) && !/kill-server/.test(line)), [], "a failed tool is never read as a passed check");
  for (const tool of ['nm -U "$binary"', 'nm -m "$binary"', 'otool -L "$binary"', 'otool -l "$binary"', 'readelf -l "$binary"', 'readelf -d "$binary"', '"$@" -V']) {
    const line = lines.find((each) => each.includes(tool));
    assert(line, `${tool} is checked`);
    assert.match(line, /^\s*[a-z]+="\$\(.*\)" \|\| fail "build_tmux_[a-z_]+: /, `${tool} is captured whole and its failure names a code`);
  }
});

test("the built tmux must start a server on a private socket, keep its session and stop, not only print its version, and a failed start still stops the server", () => {
  const text = readFileSync(SCRIPT, "utf8");
  const body = text.match(/^check_start\(\) \{\n([^]*?)\n\}$/m)?.[1];
  assert(body, "check_start is defined");
  assert.match(body, /local socket="\$work\/s"/);
  assert.match(body, /env -u TMUX "\$@" -S "\$socket" -f \/dev\/null new-session -d -s check/);
  assert.match(body, /env -u TMUX "\$@" -S "\$socket" has-session -t check/);
  assert(body.split("kill-server").length - 1 >= 3, "each failure after the start and the end all stop the server");
  assert.match(body, /build_tmux_start_failed/);
  assert.match(text, /^check_macos\(\) \{\n[^]*?check_start \/usr\/bin\/arch "-\$arch" "\$binary"[^]*?\n\}$/m, "under the built architecture, as -V runs");
  assert.match(text, /^check_linux\(\) \{\n[^]*?check_start "\$binary"[^]*?\n\}$/m);
  assert.equal(pin(text, "SOCKET_PATH_MAX"), "103", "macOS sun_path holds 104 bytes with the NUL");
});

test("the tmux build refuses a work folder whose private socket path would not fit in a unix socket address, before it downloads anything", () => {
  const root = mkdtempSync(join(tmpdir(), "build-tmux-"));
  try {
    const work = join(root, "w".repeat(Math.max(1, 103 - root.length)));
    const run = refused(["--target", "aarch64-apple-darwin", "--work", work, "--out", join(root, "crazyagent-tmux")]);
    assert.equal(run.status, 2);
    assert.match(run.stderr, /build_tmux_work_too_long/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
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
