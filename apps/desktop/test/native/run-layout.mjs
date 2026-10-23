import { mkdirSync } from "node:fs";
import { join } from "node:path";

export function createRunDirectories(evidence) {
  for (const name of ["home", "tmp", "webdriver"]) mkdirSync(join(evidence, name), { mode: 0o700 });
}
