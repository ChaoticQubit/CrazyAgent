export interface ModifierEvent {
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  target?: EventTarget | null;
}

/** Whether this window runs on macOS, where shortcuts use Cmd; Windows and Linux use Ctrl.
 * Input: the platform name, by default the window's. Output: true on macOS. */
export function isMac(platform: string = typeof navigator === "undefined" ? "" : navigator.platform): boolean {
  return /^Mac/i.test(platform);
}

/** Whether a key event holds the app's shortcut key and nothing else: Cmd on macOS, Ctrl on
 * Windows and Linux, never Alt. Off Mac, keys typed into a terminal stay with the terminal.
 * Inputs: the event and whether this is macOS. Output: true when the shortcut applies. */
export function shortcutKey(event: ModifierEvent, mac: boolean = isMac()): boolean {
  if (event.altKey) return false;
  if (mac) return event.metaKey && !event.ctrlKey;
  const inTerminal = typeof Element !== "undefined" && event.target instanceof Element && event.target.closest(".xterm") !== null;
  return event.ctrlKey && !event.metaKey && !inTerminal;
}

/** Shows a shortcut with this platform's key name: Cmd on macOS, Ctrl elsewhere.
 * Inputs: the text written with Cmd and whether this is macOS. Output: the text to show. */
export function keyLabel(text: string, mac: boolean = isMac()): string {
  return mac ? text : text.replace(/\bCmd\b/g, "Ctrl");
}

export interface ComposerKeyEvent extends ModifierEvent {
  key: string;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
}

/** Whether a key press in a message composer sends the message: Enter, alone or with the
 * app's shortcut key. Shift+Enter inserts a new line instead, and Enter never sends while an
 * input method is composing, since that Enter picks the composed text; WebKit reports it with
 * key code 229. Inputs: the native key event and whether this is macOS. Output: true to send. */
export function sendsMessage(event: ComposerKeyEvent, mac: boolean = isMac()): boolean {
  if (event.key !== "Enter" || event.shiftKey || event.isComposing || event.keyCode === 229) return false;
  return (!event.metaKey && !event.ctrlKey && !event.altKey) || shortcutKey(event, mac);
}
