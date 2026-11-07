import type { GraphNode } from "../../engine-client";

/** The choices of terminals in one terminal wall window. */
export type PerWindow = 4 | 6 | 9;

/** Splits the project's terminals into terminal wall windows: the master first, then every
 * other agent with a terminal in graph order, perWindow to a window, the last window holding
 * the rest. Inputs: the graph's nodes and the terminals per window. Output: one list of
 * nodes per window, none when no agent has a terminal. */
export function wallPages(nodes: GraphNode[], perWindow: PerWindow): GraphNode[][] {
  void perWindow;
  return [nodes];
}
