import type { RemovalPreview } from "../../api-projects";

export type RemoveChoice = "dashboard" | "everything";

/** The ways a project can be removed. Input: the engine's preview, or null while it loads.
 * Output: Remove from dashboard first, then Delete everything when every folder is inside the
 * projects home. */
export function removeChoices(preview: RemovalPreview | null): RemoveChoice[] {
  void preview;
  return ["dashboard"];
}

/** Whether Delete everything may be sent. Inputs: the preview and the typed name. Output: true
 * only when the preview allows a delete and the typed name is the project's exact name. */
export function deleteReady(preview: RemovalPreview | null, typed: string): boolean {
  void preview;
  void typed;
  return false;
}

/** What Delete everything would lose, one plain line each. Input: the preview. Output: the
 * running agents, the open change sets and each agent's unlanded work. */
export function lossLines(preview: RemovalPreview): string[] {
  void preview;
  return [];
}

/** The remove dialog's body. Inputs: the preview, the picked choice, the typed name, whether a
 * removal is running and the change handlers. Output: the choices, the loss list and the name box. */
export function RemoveDialogView(props: {
  preview: RemovalPreview | null;
  choice: RemoveChoice;
  typed: string;
  pending: boolean;
  onChoice: (choice: RemoveChoice) => void;
  onTyped: (typed: string) => void;
}) {
  void props;
  return <div />;
}
