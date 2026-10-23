let pending: string | null = null;

/** Keeps one message for the next screen to show, for example after creating a project.
 * Input: the message. Output: none. */
export function flash(message: string): void {
  pending = message;
}

/** Takes the kept message once.
 * Input: none. Output: the message, or null when there is none. */
export function takeFlash(): string | null {
  const message = pending;
  pending = null;
  return message;
}

/** Moves the app to a hash route, the way the shell's router reads it.
 * Input: a path such as "/projects/abc/overview". Output: none. */
export function go(path: string): void {
  if (typeof window !== "undefined") window.location.hash = `#${path}`;
}
