import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { constants, closeSync, fstatSync, lstatSync, openSync,
  readSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pollDelay } from "node:timers/promises";
import { performance } from "node:perf_hooks";

const marker = "native-refresh-page: ";
const urls = new Set(["tauri://localhost", "tauri://localhost/", "tauri://localhost/index.html",
  "http://tauri.localhost/", "http://tauri.localhost/index.html"]);

function decimal(value) {
  assert(typeof value === "string" && /^[0-9]{1,24}$/.test(value), "bounded native time");
  return BigInt(value);
}

export function parseNavigation(texts, run) {
  assert(/^[A-Za-z0-9_-]{1,64}$/.test(run), "bounded native run");
  const state = { identity: null, url: null, sequence: 0, generation: 0,
    finished: 0, pending: false, partial: false };
  let elapsed = -1n;
  for (const text of texts) {
    assert(Buffer.byteLength(text) <= 4 * 1024 * 1024, "native stream byte bound");
    const lines = text.split("\n");
    const tail = lines.pop();
    assert(Buffer.byteLength(tail) <= 1024, "native partial line bound");
    state.partial ||= tail.length > 0;
    for (const line of lines) {
      const offset = line.indexOf(marker);
      if (offset < 0) continue;
      assert(/(?:^|\s)\[Tauri:Backend(?::0)?\] $/.test(line.slice(0, offset)),
        "native backend source required");
      const body = line.slice(offset + marker.length);
      assert(Buffer.byteLength(body) <= 768, "native record byte bound");
      const record = JSON.parse(body);
      assert(record && !Array.isArray(record) && record.schema === 1 && !record.invalid,
        "invalid native observation");
      assert.equal(record.run, run, "native run mismatch");
      assert.equal(record.window, "main", "native window mismatch");
      assert(Number.isInteger(record.pid) && record.pid > 0 && record.pid <= 0xffffffff,
        "native process identity bound");
      const started = decimal(record.app_started_ns);
      assert(started > 0n, "native app identity required");
      const identity = `${record.pid}:${started}`;
      state.identity ??= identity;
      assert.equal(identity, state.identity, "native app identity changed");
      assert(urls.has(record.url), "native asset URL required");
      state.url ??= record.url;
      assert.equal(record.url, state.url, "native asset URL changed");
      assert.equal(record.sequence, ++state.sequence, "native sequence gap");
      assert(state.sequence <= 4, "unexpected extra native navigation");
      const tick = decimal(record.elapsed_ns);
      assert(tick >= elapsed, "native time regressed");
      assert(decimal(record.wall_ns) > 0n, "native wall identity required");
      elapsed = tick;
      if (record.phase === "Started") {
        assert.equal(state.pending, false, "native navigation overlap");
        assert.equal(record.generation, ++state.generation, "native generation gap");
        state.pending = true;
      } else {
        assert.equal(record.phase, "Finished", "invalid native phase");
        assert(state.pending, "native finish without commit");
        assert.equal(record.generation, state.generation, "native finish generation mismatch");
        state.pending = false;
        state.finished += 1;
      }
    }
  }
  return state;
}

function privateDirectory(path, uid) {
  const info = lstatSync(path, { bigint: true });
  assert(info.isDirectory() && info.uid === BigInt(uid) && (info.mode & 0o077n) === 0n,
    "private owned native log directory required");
  return `${info.dev}:${info.ino}`;
}

function readLog(path, uid, remaining, previous) {
  const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const before = fstatSync(descriptor, { bigint: true });
    const identity = `${before.dev}:${before.ino}`;
    assert(before.isFile() && before.uid === BigInt(uid) && (before.mode & 0o077n) === 0n,
      "private regular owned native log required");
    assert(before.size <= BigInt(remaining), "native aggregate byte bound");
    if (previous) {
      assert.equal(identity, previous.identity, "native log replaced");
      assert(before.size >= previous.size, "native log truncated");
      assert(before.mtimeNs >= previous.mtimeNs, "native log time regressed");
    }
    const size = Number(before.size);
    const bytes = Buffer.alloc(size + 1);
    let used = 0;
    while (used < bytes.length) {
      const count = readSync(descriptor, bytes, used, bytes.length - used, used);
      if (count === 0) break;
      used += count;
    }
    const after = fstatSync(descriptor, { bigint: true });
    const linked = lstatSync(path, { bigint: true });
    assert.equal(used, size, "native log changed while reading");
    assert.equal(after.size, before.size, "native log size changed");
    assert.equal(after.mtimeNs, before.mtimeNs, "native log timestamp changed");
    assert.equal(after.ctimeNs, before.ctimeNs, "native log metadata changed");
    assert.equal(`${linked.dev}:${linked.ino}`, identity, "native log path changed");
    assert(!linked.isSymbolicLink(), "native log path became symlink");
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, used));
    if (previous) assert(text.startsWith(previous.text), "native log prefix changed");
    return { text, identity, size: before.size, mtimeNs: before.mtimeNs };
  } finally {
    closeSync(descriptor);
  }
}

export function createNavigationReader(evidence, run) {
  const uid = process.getuid();
  const rootIdentity = privateDirectory(evidence, uid);
  const directory = join(evidence, "webdriver");
  let directoryIdentity;
  const files = new Map();
  return () => {
    assert.equal(privateDirectory(evidence, uid), rootIdentity, "native evidence replaced");
    let identity;
    try { identity = privateDirectory(directory, uid); } catch (error) {
      if (error.code === "ENOENT" && !directoryIdentity) return parseNavigation([], run);
      throw error;
    }
    directoryIdentity ??= identity;
    assert.equal(identity, directoryIdentity, "native log directory replaced");
    const names = readdirSync(directory);
    assert(names.length <= 16, "native log file count bound");
    for (const name of files.keys()) assert(names.includes(name), "native log disappeared");
    let remaining = 4 * 1024 * 1024;
    const texts = [];
    for (const name of names.sort()) {
      assert(/^wdio[A-Za-z0-9_.-]{0,120}\.log$/.test(name), "unexpected native log name");
      const record = readLog(join(directory, name), uid, remaining, files.get(name));
      remaining -= Number(record.size);
      files.set(name, record);
      texts.push(record.text);
    }
    assert.equal(privateDirectory(evidence, uid), rootIdentity, "native evidence replaced");
    assert.equal(privateDirectory(directory, uid), directoryIdentity, "native log directory replaced");
    return parseNavigation(texts, run);
  };
}

async function bounded(action, deadline) {
  const remaining = deadline - performance.now();
  assert(remaining > 0, "native refresh deadline");
  let timer;
  try {
    return await Promise.race([action(), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("native refresh deadline")), remaining);
    })]);
  } finally { clearTimeout(timer); }
}

async function navigation(read, finished, deadline, initial) {
  for (let poll = 0; poll < 1000; poll += 1) {
    assert(performance.now() < deadline, "native refresh deadline");
    const state = read();
    assert(state.generation <= finished, "unexpected native generation");
    if (initial) {
      assert.equal(state.identity, initial.identity, "native app identity changed");
      assert.equal(state.url, initial.url, "native asset URL changed");
    }
    if (state.finished === finished && !state.pending && !state.partial) return state;
    await bounded(() => pollDelay(20), deadline);
  }
  throw new Error("native refresh poll bound");
}

function stable(state, expected) {
  assert.deepEqual(state, expected, "native navigation changed during document readiness");
}

export async function refreshBarrier({ read, mark, original, ready, timeoutMs = 15000 }) {
  assert(Number.isInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 15000,
    "bounded refresh timeout required");
  const deadline = performance.now() + timeoutMs;
  const initial = await navigation(read, 1, deadline);
  await bounded(mark, deadline);
  stable(read(), initial);
  const result = await bounded(original, deadline);
  const replacement = await navigation(read, 2, deadline, initial);
  for (let poll = 0; poll < 1000; poll += 1) {
    assert(performance.now() < deadline, "native refresh deadline");
    const actual = await bounded(ready, deadline);
    assert.equal(typeof actual, "boolean", "actual document readiness must be boolean");
    stable(read(), replacement);
    if (actual) return result;
    await bounded(() => pollDelay(20), deadline);
  }
  throw new Error("native document readiness poll bound");
}

export function markDocument(token) {
  if (Object.hasOwn(document, "__crazyagentRefreshMarker")) {
    throw new Error("document already marked");
  }
  Object.defineProperty(document, "__crazyagentRefreshMarker", { value: token });
  return document.__crazyagentRefreshMarker === token;
}

export function documentReadiness(token, url) {
  if (Object.hasOwn(document, "__crazyagentRefreshMarker")) {
    if (document.__crazyagentRefreshMarker !== token) {
      throw new Error("document marker mismatch");
    }
    return false;
  }
  if (location.href !== url) throw new Error("replacement document URL mismatch");
  const owner = document.querySelector('[data-testid="connect-owner"]');
  return document.readyState === "complete" && Boolean(document.querySelector("#root .app-shell")) &&
    Boolean(owner && !owner.disabled && owner.getClientRects().length) &&
    document.querySelector(".sidebar-footer")?.textContent.trim() === "Owner connected";
}

export function installRefresh(browser, evidence, run, timeoutMs = 15000) {
  assert.equal(typeof browser.overwriteCommand, "function");
  const read = createNavigationReader(evidence, run);
  let dispatched = false;
  browser.overwriteCommand("refresh", async function (original, ...args) {
    assert.equal(dispatched, false, "exactly one native refresh permitted");
    dispatched = true;
    const token = randomUUID();
    let url;
    return refreshBarrier({ read, timeoutMs,
      mark: async () => {
        url = read().url;
        assert.equal(await this.execute(markDocument, token), true, "old document marker required");
      },
      original: () => original(...args),
      ready: () => this.execute(documentReadiness, token, url),
    });
  });
}
