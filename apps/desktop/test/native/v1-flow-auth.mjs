import assert from "node:assert/strict";
import { existsSync, lstatSync, readdirSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";

export const BLOCKED_SIGN_IN = "BLOCKED: set CRAZYAGENT_MANAGED_CODEX_HOME to the folder of a signed-in managed Codex profile that holds auth.json";
const SIGN_IN = "auth.json";
const LEDGER_BYTES_MAX = 16 * 1024;

function regularFile(path) {
  try {
    return lstatSync(path).isFile();
  } catch {
    return false;
  }
}

function plainDirectory(path) {
  try {
    return lstatSync(path).isDirectory();
  } catch {
    return false;
  }
}

/** Finds the signed-in managed Codex home the V1 flow borrows. Input: the environment.
 * Output: the absolute folder holding auth.json; throws the plain BLOCKED message when the
 * variable is unset, relative, or names a folder without a regular auth.json. */
export function managedCodexHome(env) {
  const home = env.CRAZYAGENT_MANAGED_CODEX_HOME;
  if (!home || !isAbsolute(home) || !regularFile(join(home, SIGN_IN))) throw new Error(BLOCKED_SIGN_IN);
  return home;
}

/** Lists the managed profile homes an engine made under its root. Input: the engine root.
 * Output: the absolute `native-profiles/<digest>/home` folders, sorted; empty when none. */
export function engineProfileHomes(root) {
  const base = join(root, "native-profiles");
  if (!plainDirectory(base)) return [];
  return readdirSync(base).filter((name) => /^[0-9a-f]{64}$/.test(name)).map((name) => join(base, name, "home"))
    .filter(plainDirectory).sort();
}

/** Moves the managed sign-in into the engine's only profile home, recording the move first so
 * it can be returned. Moving, not copying, keeps one live copy of a rotating sign-in. Inputs:
 * the source home, the engine root and the private ledger path. Output: the target home. */
export function borrowSignIn(source, root, ledger) {
  assert(isAbsolute(source) && isAbsolute(root) && isAbsolute(ledger), "absolute borrow paths required");
  const homes = engineProfileHomes(root);
  if (homes.length !== 1) throw new Error(`exactly one engine profile home, found ${homes.length}`);
  const target = homes[0];
  if (existsSync(ledger) || existsSync(join(target, SIGN_IN))) throw new Error("sign-in already borrowed");
  if (!regularFile(join(source, SIGN_IN))) throw new Error(BLOCKED_SIGN_IN);
  writeFileSync(ledger, JSON.stringify({ source, target }), { flag: "wx", mode: 0o600 });
  renameSync(join(source, SIGN_IN), join(target, SIGN_IN));
  return target;
}

/** Moves a borrowed sign-in back to its source home; call it only after the engine stopped.
 * Input: the ledger path. Output: true when a sign-in was returned, false when none was
 * borrowed. Throws, keeping the ledger, when the source already has a sign-in or the
 * borrowed one is missing. */
export function returnSignIn(ledger) {
  if (!existsSync(ledger)) return false;
  assert(statSync(ledger).size <= LEDGER_BYTES_MAX, "borrow ledger byte bound");
  const { source, target } = JSON.parse(readFileSync(ledger, "utf8"));
  assert(typeof source === "string" && isAbsolute(source) && typeof target === "string" && isAbsolute(target), "closed borrow ledger required");
  const back = join(source, SIGN_IN);
  if (existsSync(back)) throw new Error(`sign-in not returned: ${back} already exists`);
  if (!regularFile(join(target, SIGN_IN))) throw new Error(`borrowed sign-in missing from ${target}`);
  renameSync(join(target, SIGN_IN), back);
  unlinkSync(ledger);
  return true;
}
