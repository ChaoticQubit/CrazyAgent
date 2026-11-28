import { INPUT_DEFAULT, inputChoice, type InputChoice } from "./screens/team/terminal-io";
import { rendererChoice, rendererDefault, type RendererChoice } from "./screens/team/terminal-renderer";
import { choiceListeners } from "./choice-listeners";

const TERMINAL_INPUT_KEY = "crazyagent.terminal-input";
const TERMINAL_RENDERER_KEY = "crazyagent.terminal-renderer";

/** Reads where the owner types to every agent's terminal, as kept in this app (not in the
 * engine). The old Master input key is ignored on purpose: a live trial left Both in it.
 * Output: the choice; the default when storage is unavailable or holds something else. */
export function readTerminalInput(): InputChoice {
  try {
    return inputChoice(window.localStorage.getItem(TERMINAL_INPUT_KEY));
  } catch {
    return INPUT_DEFAULT;
  }
}

const inputHeard = choiceListeners(TERMINAL_INPUT_KEY);

/** Remembers where the owner types to every agent's terminal and tells this window's
 * listeners at once; other windows hear it through the storage event. Input: the choice.
 * Output: true once stored; false when storage refused it, and then nobody is told. */
export function saveTerminalInput(choice: InputChoice): boolean {
  try {
    window.localStorage.setItem(TERMINAL_INPUT_KEY, choice);
  } catch {
    return false;
  }
  inputHeard.tell();
  return true;
}

/** Calls `listener` after each Terminal input choice, made in this window or in another
 * window of the app, so every copy of the setting shows the stored choice (#406 F1). Input:
 * the listener. Output: the call that stops it. */
export function onTerminalInput(listener: () => void): () => void {
  return inputHeard.listen(listener);
}

/** Reads how every terminal draws, as kept in this app (not in the engine). Output: the
 * choice; the platform's default when storage is unavailable or holds something else. */
export function readTerminalRenderer(): RendererChoice {
  try {
    return rendererChoice(window.localStorage.getItem(TERMINAL_RENDERER_KEY));
  } catch {
    return rendererDefault();
  }
}

const rendererHeard = choiceListeners(TERMINAL_RENDERER_KEY);

/** Remembers how every terminal draws and tells this window's listeners at once; other
 * windows hear it through the storage event. Input: the choice. Output: true once stored;
 * false when storage refused it, and then nobody is told. */
export function saveTerminalRenderer(choice: RendererChoice): boolean {
  try {
    window.localStorage.setItem(TERMINAL_RENDERER_KEY, choice);
  } catch {
    return false;
  }
  rendererHeard.tell();
  return true;
}

/** Calls `listener` after each Terminal renderer choice, made in this window or in another
 * window of the app. Input: the listener. Output: the call that stops it. */
export function onTerminalRenderer(listener: () => void): () => void {
  return rendererHeard.listen(listener);
}
