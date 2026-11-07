import type { GraphNode } from "../../engine-client";

/** The choices of terminals in one terminal wall window. */
export type PerWindow = 4 | 6 | 9;

/** Splits the project's terminals into terminal wall windows: the master first, then every
 * other agent with a terminal in graph order, perWindow to a window, the last window holding
 * the rest. Inputs: the graph's nodes and the terminals per window. Output: one list of
 * nodes per window, none when no agent has a terminal. */
export function wallPages(nodes: GraphNode[], perWindow: PerWindow): GraphNode[][] {
  assert(perWindow === 4 || perWindow === 6 || perWindow === 9, "terminals per window is 4, 6 or 9");
  const open = nodes.filter((item) => item.terminal !== null);
  const tiles = [...open.filter((item) => item.kind === "master"), ...open.filter((item) => item.kind !== "master")];
  const pages: GraphNode[][] = [];
  for (let start = 0; start < tiles.length; start += perWindow) pages.push(tiles.slice(start, start + perWindow));
  assert(pages.flat().length === tiles.length, "every terminal takes one tile");
  return pages;
}

/** Throws when an internal invariant of the wall's page math is broken. Inputs: the
 * condition and the message. Output: none; narrows the condition to true. */
function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The most terminal wall windows one project opens, as the desktop bridge allows. */
export const WALL_WINDOWS_MAX = 8;

/** Reads a terminal wall window's project and page from its address, `#/terminals/<project>/<page>`.
 * Input: the location hash. Output: the project id and the page, 1 to WALL_WINDOWS_MAX, or
 * null for any other address. */
export function wallRoute(hash: string): { project: string; page: number } | null {
  const match = /^#\/terminals\/([^/]+)\/([1-9])$/u.exec(hash);
  if (match === null) return null;
  const page = Number(match[2]);
  if (page > WALL_WINDOWS_MAX) return null;
  try {
    const project = decodeURIComponent(match[1]);
    return project.length > 0 ? { project, page } : null;
  } catch {
    return null;
  }
}

/** The hint on Open all terminals. Inputs: how many windows the team takes and the terminals
 * per window. Output: one sentence. */
export function openAllHint(windows: number, perWindow: PerWindow): string {
  assert(Number.isInteger(windows) && windows >= 0, "a whole number of windows");
  if (windows === 0) return "No agent has a terminal yet.";
  return windows === 1 ? `Opens 1 window of up to ${perWindow} terminals.` : `Opens ${windows} windows, ${perWindow} terminals each.`;
}
