import { Channel } from "@tauri-apps/api/core";
import { call, type Result } from "../../api";

/** A native view's place: the host element's `getBoundingClientRect()` in CSS pixels of the
 * webview's viewport. */
export type NativeRect = { x: number; y: number; width: number; height: number };

/** The colours a native view draws with, each a CSS colour string (`#rgb`, `#rrggbb`,
 * `rgb()` or `rgba()`) read from the same `--terminal-*` tokens as today's panel. */
export type NativeTheme = {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
};

/** How every native view draws: the font list (the first installed family is used, at most
 * 256 bytes), the size in CSS pixels (6 to 72), whether the cursor blinks, and the colours. */
export type NativeStyle = { fontFamily: string; fontSize: number; cursorBlink: boolean; theme: NativeTheme };

/** What a native view tells its panel, in order: `ready` once with its grid; then at any time
 * `menu` (a right click, at CSS pixels in the webview), `mouse` (the pane turned mouse
 * reporting on or off), `behind` (the view scrolled off or back to the bottom); and last, at
 * most once, `ended` with a plain code. `final` is false when the agent ended or restarted and
 * a reopen may find it again, true when reopening cannot help (grant revoked, attach
 * rejected, a refused request). After `ended` the view is gone. */
export type NativeEvent =
  | { type: "ready"; cols: number; rows: number }
  | { type: "menu"; x: number; y: number }
  | { type: "mouse"; on: boolean }
  | { type: "behind"; behind: boolean }
  | { type: "ended"; code: string; final: boolean };

/** What a copy takes: the selection, or the whole output the view holds (history and screen). */
export type NativeCopy = "selection" | "output";

/** One native view to open: the agent's terminal name (`node.terminal`), the teammate pane it
 * shows or null, the label the screen reader says ("<label> terminal"), its place, its style,
 * whether it opens covered (a kept view preloaded with no host yet: it attaches and keeps its
 * terminal live but draws nothing until `cover(false)`) and the channel its events arrive on. */
export type NativeOpen = {
  name: string;
  pane: string | null;
  label: string;
  rect: NativeRect;
  style: NativeStyle;
  covered: boolean;
  events: Channel<NativeEvent>;
};

/** Whether this desktop draws terminals natively: true only in a macOS build. The command is
 * built and granted on every OS, so it answers false on Windows and Linux. Read once at start.
 * Output: the answer. */
export function nativeTerminalEnabled(): Promise<Result<boolean>> {
  return call<boolean>("desktop_native_terminal_enabled");
}

/** Opens a native view over the host element. Input: the view to open. Output: the view id
 * every other call names. */
export function openNativeTerminal(open: NativeOpen): Promise<Result<number>> {
  return call<number>("desktop_native_terminal_open", open);
}

/** Moves or resizes a view to its host element's new place; its grid follows. Inputs: the
 * view and the place. Output: ok once applied. */
export function placeNativeTerminal(view: number, rect: NativeRect): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_place", { view, rect });
}

/** Hides a view while something covers its place (a dialog, menu, listbox, tooltip or
 * popover, or a kept view parked off screen) and shows it again; a covered view keeps its
 * terminal live. Inputs: the view and whether it is covered. Output: ok once applied. */
export function coverNativeTerminal(view: number, covered: boolean): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_cover", { view, covered });
}

/** Gives a view the keyboard. Input: the view. Output: ok once it has focus. */
export function focusNativeTerminal(view: number): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_focus", { view });
}

/** Sets how every open native view draws, and every view opened later. Input: the style.
 * Output: ok once applied. */
export function styleNativeTerminals(style: NativeStyle): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_style", { style });
}

/** Copies a view's selection or its whole output to the clipboard. Inputs: the view and what
 * to copy. Output: ok once copied, or a plain code when there was nothing to copy or it was
 * too large. */
export function copyNativeTerminal(view: number, what: NativeCopy): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_copy", { view, what });
}

/** Scrolls a view back to the bottom of its output. Input: the view. Output: ok. */
export function jumpNativeTerminal(view: number): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_jump", { view });
}

/** Turns a view's own keys on or off, as the input box modes need: off, the view still shows
 * output and selects text but types nothing. Inputs: the view and whether keys go through.
 * Output: ok once applied. */
export function keysNativeTerminal(view: number, on: boolean): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_keys", { view, on });
}

/** Closes a view; the agent keeps running. Closing a view twice is fine. Input: the view.
 * Output: ok once closed. */
export function closeNativeTerminal(view: number): Promise<Result<void>> {
  return call<void>("desktop_native_terminal_close", { view });
}
