import assert from "node:assert/strict";
import { lstatSync, mkdirSync, mkdtempSync, realpathSync, rmSync, statSync } from "node:fs";
import { userInfo } from "node:os";
import { isAbsolute, join } from "node:path";

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
 * app's web storage starts empty as in a fresh home, where there is none to remove. The home's
 * keychain and every other folder stay. Input: the home. Output: nothing. */
export function clearWebStore(home) {
  const store = join(home, "Library", "WebKit");
  rmSync(store, { recursive: true, force: true });
  assert.throws(() => lstatSync(store), { code: "ENOENT" }, "no web store is left for the run");
}
