import { formatBytes } from "./screens/new-project/wizard";

/** The size line of the very large warning once the folder was measured. Input: its bytes.
 * Output: one sentence, such as "It is about 42 GB.". */
export function measuredLine(bytes: number): string {
  return `It is about ${formatBytes(bytes)}.`;
}
