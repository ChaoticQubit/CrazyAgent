import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** Runs git in a folder and stops the run when it fails. Inputs: the folder, the home git
 * reads and the git arguments. Output: git's standard output. */
function git(cwd, home, ...args) {
  const run = spawnSync("git", ["-c", "user.name=Questions Flow", "-c", "user.email=owner@example.invalid", "-c", "commit.gpgsign=false", ...args],
    { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  if (run.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${run.stderr.trim()}`);
  return run.stdout;
}

/** Makes one of the owner's existing projects: a git repository with one commit.
 * Inputs: the folder, its readme title and the git home. Output: none. */
export function gitRepo(folder, title, home) {
  mkdirSync(join(folder, "src"), { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "README.md"), `# ${title}\n\nSells house plants.\n`);
  writeFileSync(join(folder, "src", "main.rs"), "fn main() {\n    println!(\"plants\");\n}\n");
  git(folder, home, "init", "-q", "-b", "main");
  git(folder, home, "add", ".");
  git(folder, home, "commit", "-q", "-m", "Start the plant shop");
}
