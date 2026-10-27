import * as path from "node:path";

/** The button that unlocks a file in a running agent's worktree. */
export const EDIT_ANYWAY = "Edit anyway";

/**
 * Names the team agent whose worktree a workspace folder is: the engine adds each one as
 * `.../worktrees/agent-<id>`, and the engine checks the agent owns it before acting.
 * Input: the folder's file system path. Output: the terminal name `team-<id>`, or null.
 */
export function agentOf(folder: string): string | null {
  const match = /^agent-([1-9][0-9]*)$/.exec(path.basename(folder));
  if (match === null || path.basename(path.dirname(path.resolve(folder))) !== "worktrees") return null;
  return `team-${match[1]}`;
}

/**
 * Takes a file's path relative to the folder it sits in, the way git names it.
 * Inputs: the folder and the file's file system paths. Output: the `/`-separated
 * relative path, or null for the folder itself or a file outside it.
 */
export function insideFolder(folder: string, file: string): string | null {
  const relative = path.relative(folder, file);
  if (relative === "" || path.isAbsolute(relative) || relative.split(path.sep).includes("..")) return null;
  return relative.split(path.sep).join("/");
}

/**
 * The question asked before the owner edits a file an agent may be changing.
 * Input: the agent's workspace folder name. Output: the question.
 */
export function editAnywayQuestion(name: string): string {
  return `${name} is running in this worktree. Edit anyway?`;
}
