import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { linkSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { userInfo } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";

export function createRunDirectories(evidence) {
  for (const name of ["home", "tmp", "webdriver"]) mkdirSync(join(evidence, name), { mode: 0o700 });
}

/** The HOME a native run gives its app and engine: the folder CRAZYAGENT_E2E_HOME names, so a
 * gate can bring a prepared keychain (DEC-146), else the run's own fresh home. A named folder
 * must be an absolute path to an existing folder this user owns that is not the real home.
 * Inputs: the fresh home, the environment and the real home from the user database.
 * Output: { home, prepared }, or a thrown refusal naming what is wrong. */
export function chooseHome(fresh, env = process.env, realHome = userInfo().homedir) {
  const named = env.CRAZYAGENT_E2E_HOME;
  if (named === undefined || named === "") return { home: fresh, prepared: false };
  if (!isAbsolute(named)) throw new Error(`CRAZYAGENT_E2E_HOME must be an absolute path: ${named}`);
  let info;
  try {
    info = statSync(named);
  } catch {
    throw new Error(`CRAZYAGENT_E2E_HOME must name an existing folder: ${named}`);
  }
  if (!info.isDirectory()) throw new Error(`CRAZYAGENT_E2E_HOME must be a folder: ${named}`);
  if (info.uid !== process.getuid()) throw new Error(`CRAZYAGENT_E2E_HOME must be owned by this user: ${named}`);
  if (realpathSync(named) === realpathSync(realHome)) throw new Error(`CRAZYAGENT_E2E_HOME must not be the real home: ${named}`);
  return { home: named, prepared: true };
}

/** Makes one native run's projects folder: a new private Projects-XXXXXX folder in the home,
 * so runs that share a prepared home never meet an earlier run's projects or its folder.
 * Input: the home. Output: the folder's path. */
export function runProjects(home) {
  const projects = mkdtempSync(join(home, "Projects-"));
  const info = lstatSync(projects);
  assert(info.isDirectory() && !info.isSymbolicLink(), "the run's projects folder is a new folder");
  assert.equal(info.mode & 0o777, 0o700, "the run's projects folder is private");
  return projects;
}

/** Removes the WebKit folder an earlier run left in the home before the app starts, so the
 * app's web storage starts empty as in a fresh home, where there is none to remove. Library
 * must be a plain folder of the home itself, never a link, so the removal cannot reach a
 * folder outside the home; a linked WebKit loses only its link. Input: the home. Output:
 * nothing, or a thrown refusal before anything is removed. */
export function clearWebStore(home) {
  const library = join(home, "Library");
  let info;
  try {
    info = lstatSync(library);
  } catch (error) {
    if (error.code === "ENOENT") return;
    throw error;
  }
  if (info.isSymbolicLink() || !info.isDirectory()) throw new Error(`refusing to clear ${library}: not a plain folder`);
  if (realpathSync(library) !== join(realpathSync(home), "Library")) throw new Error(`refusing to clear ${library}: it resolves outside the home`);
  const store = join(library, "WebKit");
  rmSync(store, { recursive: true, force: true });
  assert.throws(() => lstatSync(store), { code: "ENOENT" }, "no web store is left for the run");
}

const LOCK_NAME = ".crazyagent-native-run.lock";
const LOCK_WAIT_MS = 60000;
const LOCK_POLL_MS = 250;
const LOCK_RETRIES_MAX = 16;

/** Takes the one native run lock of a home for this process, so two native suites never run
 * side by side on one home: a held home is waited on for waitMs, then refused naming the
 * holder; a lock whose pid is dead is taken over, one taker at a time; this process frees it
 * on exit. Inputs: the home and { label, waitMs, pollMs }. Output: the lock, whose release()
 * frees it. */
export async function lockHome(home, { label, waitMs = LOCK_WAIT_MS, pollMs = LOCK_POLL_MS } = {}) {
  assert(isAbsolute(home), "the home is an absolute path");
  assert(typeof label === "string" && label !== "", "the lock names its suite");
  assert(Number.isInteger(waitMs) && waitMs >= 0 && Number.isInteger(pollMs) && pollMs > 0, "the wait is bounded");
  const lock = join(home, LOCK_NAME);
  const mine = { pid: process.pid, started: new Date().toISOString(), label, token: randomBytes(16).toString("hex") };
  const draft = `${lock}.${mine.token}`;
  writeFileSync(draft, JSON.stringify(mine), { flag: "wx", mode: 0o600 });
  const deadline = Date.now() + waitMs;
  const attempts = Math.ceil(waitMs / pollMs) + LOCK_RETRIES_MAX;
  let held = null;
  try {
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      try {
        linkSync(draft, lock);
        held = mine;
        break;
      } catch (error) {
        if (error.code !== "EEXIST") throw error;
      }
      const holder = readLock(lock);
      if (holder === undefined) continue;
      if (holder !== null && !alive(holder.pid) && takeOver(lock, holder, draft, mine.token)) continue;
      if (Date.now() >= deadline) throw new Error(heldMessage(home, lock, holder, waitMs));
      await pause(pollMs);
    }
  } finally {
    rmSync(draft, { force: true });
  }
  if (held === null) throw new Error(heldMessage(home, lock, readLock(lock) ?? null, waitMs));
  const release = () => {
    if (readLock(lock)?.token === mine.token) rmSync(lock, { force: true });
    process.removeListener("exit", release);
  };
  process.on("exit", release);
  return { release };
}

/** Reads a home's lock. Input: the lock file. Output: its holder { pid, started, label, token },
 * undefined when there is no lock, or null when the file is not a lock this code wrote. */
function readLock(lock) {
  let text;
  try {
    text = readFileSync(lock, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
  let holder;
  try {
    holder = JSON.parse(text);
  } catch {
    return null;
  }
  const valid = holder !== null && typeof holder === "object" && Number.isInteger(holder.pid) && holder.pid > 0
    && typeof holder.started === "string" && typeof holder.label === "string" && /^[0-9a-f]{32}$/.test(holder.token ?? "");
  return valid ? holder : null;
}

/** Tells whether a process lives: signal 0 reaches it, or it belongs to another user.
 * Input: the pid. Output: the answer. */
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code !== "ESRCH";
  }
}

/** Removes a lock whose holder is dead, one taker at a time: the taker links its draft as the
 * home's takeover lock, then removes the lock only while it is still the dead holder's, so a
 * taker that read an old holder never removes a lock a faster taker made. A takeover lock left
 * by a dead taker is refused, as no taker can remove it safely. Inputs: the lock, the dead
 * holder, this taker's draft and token. Output: whether this taker did the takeover. */
function takeOver(lock, holder, draft, token) {
  const takeover = `${lock}.takeover`;
  try {
    linkSync(draft, takeover);
  } catch (error) {
    if (error.code !== "EEXIST") throw error;
    const taker = readLock(takeover);
    if (taker === null || (taker !== undefined && !alive(taker.pid))) throw new Error(deadTakeoverMessage(takeover, taker));
    return false;
  }
  try {
    if (readLock(lock)?.token === holder.token) rmSync(lock, { force: true });
  } finally {
    if (readLock(takeover)?.token === token) rmSync(takeover, { force: true });
  }
  assert.notEqual(readLock(takeover)?.token, token, "this taker leaves no takeover lock");
  return true;
}

/** The plain refusal for a takeover lock whose taker died midway: who left it and that it may be
 * removed by hand once no native run starts on the home. Inputs: the takeover lock and its
 * taker, null when unreadable. Output: the message. */
function deadTakeoverMessage(takeover, taker) {
  const who = taker ? `pid ${taker.pid} (${taker.label}), started ${taker.started}, which has ended` : "a file that cannot be read";
  return `The native test home's takeover lock ${takeover} was left by ${who}. No suite removes it, as a half-done takeover cannot be told from a live one. Remove ${takeover} if no native run is starting on this home.`;
}

/** The plain refusal for a held home: who holds it, since when, how long this suite waited,
 * and when the lock may be removed by hand. Inputs: the home, the lock file, its holder (null
 * when unreadable) and the wait. Output: the message. */
function heldMessage(home, lock, holder, waitMs) {
  const who = holder ? `pid ${holder.pid} (${holder.label}), started ${holder.started}` : "a lock that cannot be read";
  return `The native test home ${home} is in use by ${who}. Waited ${Math.round(waitMs / 1000)} s; native suites never run side by side on one home. Run this suite once that one ends, or remove ${lock} if no native run holds it.`;
}
