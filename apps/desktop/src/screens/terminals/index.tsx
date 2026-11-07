import type { GraphNode } from "../../engine-client";
import type { PerWindow } from "./pages";

/** One terminal wall window's tiles. Inputs: the project id, the tiles of this window, the
 * whole graph, the terminals per window and the stop action. Output: the grid. */
export function WallGrid(props: { projectId: string; tiles: GraphNode[]; nodes: GraphNode[]; perWindow: PerWindow; onStop: (node: GraphNode) => void }) {
  void props;
  return null;
}
