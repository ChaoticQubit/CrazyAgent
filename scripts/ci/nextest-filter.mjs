import { pathToFileURL } from "node:url";

const TARGET = /^[A-Za-z0-9_]+$/;
const TARGET_ROW = /^[A-Za-z0-9_]+\/[A-Za-z0-9_]+$/;
const LIBRARY_ROW = /^[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)+$/;

/**
 * Turns one package's exclusion list, in the Makefile forms `target`, `target/row` and
 * `module::row`, into nextest filterset terms. Input: the package name and the list text.
 * Output: one term per entry; an entry in no form or listed twice throws.
 */
function terms(name, list) {
  const entries = list.split(/\s+/).filter(Boolean);
  const seen = new Set();
  return entries.map((entry) => {
    if (seen.has(entry)) throw new Error(`${name} exclusion ${entry} is listed twice`);
    seen.add(entry);
    if (TARGET.test(entry)) return `binary_id(=${name}::${entry})`;
    if (TARGET_ROW.test(entry)) {
      const [target, row] = entry.split("/");
      return `(binary_id(=${name}::${target}) & test(=${row}))`;
    }
    if (LIBRARY_ROW.test(entry)) return `(package(=${name}) & test(=${entry}))`;
    throw new Error(`${name} exclusion ${JSON.stringify(entry)} is no target, target/row or module::row`);
  });
}

/**
 * Builds the nextest filterset that runs everything except the engine and desktop exclusions.
 * Inputs: the crazyagent list and the crazyagent-desktop list, whitespace separated.
 * Output: `all()` when both are empty, else `not (term | term ...)`.
 */
export function filterset(engine, desktop) {
  const all = [...terms("crazyagent", engine), ...terms("crazyagent-desktop", desktop)];
  return all.length === 0 ? "all()" : `not (${all.join(" | ")})`;
}

/**
 * Prints the filterset for FARM_ENGINE_EXCLUDE and FARM_DESKTOP_EXCLUDE, both required
 * (empty allowed). Input: the process environment. Output: one line on stdout.
 */
function main(env) {
  for (const name of ["FARM_ENGINE_EXCLUDE", "FARM_DESKTOP_EXCLUDE"]) {
    if (env[name] === undefined) throw new Error(`${name} is not set`);
  }
  process.stdout.write(`${filterset(env.FARM_ENGINE_EXCLUDE, env.FARM_DESKTOP_EXCLUDE)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main(process.env);
