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
