/** Where a revealed path sits: the index of its workspace folder and its parts below it. */
export interface RevealPlace {
  index: number;
  parts: string[];
}

/**
 * Finds the workspace folder a path the engine revealed sits in, the deepest one when
 * folders nest, as agent worktrees do inside the project folder, and the path's parts
 * below it. Inputs: the folders' paths, in workspace order, and the absolute path.
 * Output: the place, or undefined for a path outside every folder or with an empty,
 * `.` or `..` part.
 */
export function revealPlace(folders: readonly string[], path: string): RevealPlace | undefined {
  let found: RevealPlace | undefined;
  let depth = -1;
  folders.forEach((folder, index) => {
    const base = folder.length > 1 && folder.endsWith("/") ? folder.slice(0, -1) : folder;
    if (path !== base && !path.startsWith(`${base}/`)) return;
    if (base.length <= depth) return;
    depth = base.length;
    const rest = path.slice(base.length + 1);
    found = { index, parts: rest === "" ? [] : rest.split("/") };
  });
  if (found?.parts.some((part) => part === "" || part === "." || part === "..")) return undefined;
  return found;
}
