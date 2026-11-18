import type { Result } from "../../api";

/** How this build draws agent terminals: by the webview terminal (Windows and Linux in 2.4.3),
 * by a native view (a macOS build, DEC-203), or not at all because the desktop's answer could
 * not be read, whose plain message the panel shows in place of any terminal. */
export type TerminalKind = { kind: "web" } | { kind: "native" } | { kind: "failed"; message: string };

let answer: TerminalKind = { kind: "web" };

/** Reads, once at desktop start and before the first render, whether this build draws every
 * terminal natively. A failed read is kept as failed, so no panel falls back to the webview
 * terminal on a build that has none. Input: the desktop's answer. Output: once it is kept. */
export async function loadTerminalKind(read: () => Promise<Result<boolean>>): Promise<void> {
  const result = await read();
  if (result.kind === "ok") answer = result.value === true ? { kind: "native" } : { kind: "web" };
  else answer = { kind: "failed", message: result.kind === "error" ? result.message : "The terminal is not ready yet." };
}

/** How this build draws agent terminals, as `loadTerminalKind` kept it. Output: the kind. */
export function terminalKind(): TerminalKind {
  return answer;
}
