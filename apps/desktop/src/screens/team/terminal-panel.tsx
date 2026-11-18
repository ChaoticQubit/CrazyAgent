import { Channel } from "@tauri-apps/api/core";
import type { FitAddon } from "@xterm/addon-fit";
import type { WebglAddon } from "@xterm/addon-webgl";
import type { IDisposable, ITerminalOptions, Terminal } from "@xterm/xterm";
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type Ref } from "react";
import { attachTerminal, detachTerminal, loadTeamGraph, promptTerminal, resizeTerminal, typeIntoTerminal, type Result } from "../../api";
import type { GraphNode, TeamGraph, TerminalMessage } from "../../engine-client";
import type { InputChoice } from "./terminal-io";
import { afterFirstPaint, onTerminalRenderer, readLastProject, readTerminalInput, readTerminalRenderer, type Loaded, type ProjectRow } from "../../state";
import { Button, Icon, NotReady, StatusBadge, isMac, sendsMessage } from "../../ui";
import { displayText } from "../conversation";
import type { Seed } from "../project/console-model";
import { nodeRole, nodeRunning, nodeStatus } from "./graph";
import { HIDDEN_FLUSH_MS, HiddenOutput, KEPT_MAX, KeptSet, keyId, readSaved, resized, shownFirst, writeSaved, type Held, type KeptKey, type SavedView } from "./kept-terminals";
import { autoStartFailed, gpuStartFailed, gpuWanted, probeGpu, rendererChosen } from "./terminal-renderer";
import { connectingLine, copyKey, drawOnGpu, encodeBinary, encodeTyped, endCopy, followTry, inputLanes, inputMode, jobQueue, keyOverride, menuSpot, muteQueries, outputText, pasteText, promptText, readableScreen, seedText, selectNote, settleAfterWrites, terminalEvent, terminalSize, type GpuDrawing } from "./terminal-io";

const COPIED_MS = 2000;
const COPY_REFUSED = "The clipboard refused the copy.";
const FOLLOW_MS = 1000;
const RESIZE_PAUSE_MS = 120;
const SCROLLBACK_LINES = 5000;
const STOPPED_TYPING = "Typing stopped reaching this agent. Open the terminal again to reconnect.";

type View = { kind: "connecting"; reason?: string; tries?: number } | { kind: "live" } | { kind: "ended"; copy: string } | { kind: "not_ready" };
type ViewState = { view: View; behind: boolean; mouse: boolean };
type Link = { live: boolean; openedAt: number | null; serial: number | null; closed: boolean; wrote: () => () => void; release: () => void };

const IDLE: ViewState = { view: { kind: "connecting" }, behind: false, mouse: false };

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function token(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** The view's xterm settings: the app's mono font, the terminal tokens for every ANSI color
 * and the scrollbar, Option as Meta, Option-drag selecting text even while a full-screen app
 * reads the mouse, a right-click that keeps the selection for the menu's Copy instead of
 * picking the word under the pointer, and `SCROLLBACK_LINES` of scrollback for the history
 * the engine sends on attach. Output: the options for a new terminal. */
export function terminalOptions(): ITerminalOptions {
  return {
    cursorBlink: true,
    fontFamily: token("--mono", "ui-monospace, Menlo, monospace"),
    fontSize: 13,
    macOptionIsMeta: true,
    macOptionClickForcesSelection: true,
    rightClickSelectsWord: false,
    scrollback: SCROLLBACK_LINES,
    theme: {
      background: token("--terminal-bg", "#161513"),
      foreground: token("--terminal-fg", "#eeeae2"),
      cursor: token("--terminal-cursor", "#ff6a45"),
      cursorAccent: token("--terminal-bg", "#161513"),
      selectionBackground: token("--terminal-selection", "rgba(255, 106, 69, 0.32)"),
      scrollbarSliderBackground: token("--terminal-scrollbar", "rgba(238, 234, 226, 0.1)"),
      scrollbarSliderHoverBackground: token("--terminal-scrollbar-hover", "rgba(238, 234, 226, 0.22)"),
      scrollbarSliderActiveBackground: token("--terminal-scrollbar-hover", "rgba(238, 234, 226, 0.22)"),
      black: token("--terminal-black", "#2a2723"),
      red: token("--terminal-red", "#f2867e"),
      green: token("--terminal-green", "#6cc08e"),
      yellow: token("--terminal-yellow", "#e3b45e"),
      blue: token("--terminal-blue", "#8fb3d9"),
      magenta: token("--terminal-magenta", "#d4a0c8"),
      cyan: token("--terminal-cyan", "#7fc4bd"),
      white: token("--terminal-white", "#d8d2c6"),
      brightBlack: token("--terminal-bright-black", "#8a8477"),
      brightRed: token("--terminal-bright-red", "#ff9d94"),
      brightGreen: token("--terminal-bright-green", "#8fd6aa"),
      brightYellow: token("--terminal-bright-yellow", "#f0c77a"),
      brightBlue: token("--terminal-bright-blue", "#a9c6e6"),
      brightMagenta: token("--terminal-bright-magenta", "#e2b8d8"),
      brightCyan: token("--terminal-bright-cyan", "#9dd6cf"),
      brightWhite: token("--terminal-bright-white", "#f6f2ea"),
    },
  };
}

/** The pane a terminal view shows instead of the whole terminal: a native teammate known
 * only by its pane in its lead's terminal. Input: the node. Output: the pane id, or null. */
export function teammatePane(node: GraphNode): string | null {
  return node.kind === "teammate" && typeof node.agent !== "number" && node.pane ? node.pane : null;
}

/** The note under a live terminal: who reads what the owner types. A native teammate known
 * only by its pane gets it in that pane, with no master told. Only a terminal with a message
 * box under it speaks of that box. Inputs: the node and whether the box is there. Output: the
 * sentences. */
export function typingNote(node: GraphNode, box: boolean): string {
  if (node.kind === "master") return "Typing goes straight to the master.";
  if (teammatePane(node)) return "Typing goes straight to this teammate's pane and is not kept or told to the master.";
  const kept = "Typing goes straight to this agent and is not kept.";
  return box ? `${kept} Only a prompt you send below is also told to its master.` : kept;
}

/** The status line's note on where the owner's typing goes. Inputs: the node and the owner's
 * Terminal input. Output: the sentences. */
export function inputNote(node: GraphNode, input: InputChoice): string {
  if (input === "box") return `The terminal shows ${node.label}; type in the message box below.`;
  if (input === "terminal") return `${typingNote(node, false)} Enter sends, Shift+Enter adds a line.`;
  return typingNote(node, true);
}

/** A live terminal's whole status line: where typing goes and, while the app in the terminal
 * reads the mouse, how to select text, for every harness alike (DEC-141). Inputs: the node,
 * the owner's Terminal input, whether the app reads the mouse and whether this is a Mac.
 * Output: the sentences. */
export function liveNote(node: GraphNode, input: InputChoice, mouse: boolean, mac: boolean = isMac()): string {
  return mouse ? `${inputNote(node, input)} ${selectNote(mac)}` : inputNote(node, input);
}

/** The composer's label and placeholder for one agent. Input: the node. Output: both texts. */
export function promptCopy(node: GraphNode): { label: string; placeholder: string } {
  if (node.kind === "master") return { label: "Prompt the master", placeholder: "Tell the master what to do next" };
  if (teammatePane(node)) return { label: `Prompt ${node.label}`, placeholder: `Tell ${node.label} what to do. It goes to its pane only.` };
  return { label: `Prompt ${node.label}`, placeholder: `Tell ${node.label} what to do. Its master is told what you send.` };
}

const SEND_HELP = "Sends the text as one message, then Enter. Enter to send, Shift+Enter for a new line.";

/** The prompt box under an agent's terminal: Enter sends the text as one message, Shift+Enter
 * adds a line. The box always takes typing and keeps its draft, as a master turn's terminal
 * ends after each turn; only sending waits for a live terminal. `compact` is the mockup's
 * one-line "Type to X..." box with Send beside it, where only a reason it can't send yet shows.
 * Inputs: the node, whether the terminal is live, the send action, which says whether the
 * prompt was queued, the layout and a ref to the box. Output: the form. */
export function Composer({ node, live, onSend, compact = false, box }: {
  node: GraphNode; live: boolean; onSend: (text: string) => boolean; compact?: boolean; box?: Ref<HTMLTextAreaElement>;
}) {
  const [text, setText] = useState("");
  const id = `tg-prompt-${node.id}`;
  const copy = promptCopy(node);
  const check = promptText(text);
  const blocked = !live ? "Sends once the terminal is live." : check.kind === "too_long" ? check.reason : null;
  const send = () => {
    if (!live || check.kind !== "ok") return;
    if (onSend(check.text)) setText("");
  };
  const input = <textarea ref={box} id={id} className="pj-input tg-composer-input" rows={compact ? 1 : 2} value={text}
    placeholder={displayText(compact ? `Type to ${node.label}...` : copy.placeholder)}
    aria-describedby={compact && blocked ? `${id}-help ${id}-note` : `${id}-help`}
    onChange={(event) => setText(event.target.value)}
    onKeyDown={(event) => { if (sendsMessage(event.nativeEvent)) { event.preventDefault(); send(); } }} />;
  const submit = <Button type="submit" variant="primary" size="small" icon="send" disabled={!live || check.kind !== "ok"}>Send</Button>;
  return <form className={compact ? "tg-composer compact" : "tg-composer"} onSubmit={(event) => { event.preventDefault(); send(); }}>
    <label htmlFor={id} className="pj-sr">{displayText(copy.label)}</label>
    {compact ? <>
      <div className="tg-composer-row">{input}{submit}</div>
      <span className="visually-hidden" id={`${id}-help`}>{displayText(`${copy.placeholder} ${SEND_HELP}`)}</span>
      {blocked && <span className="tg-terminal-note" id={`${id}-note`}>{blocked}</span>}
    </> : <>
      {input}
      <div className="tg-composer-foot">
        <span className="tg-terminal-note" id={`${id}-help`}>{blocked ?? SEND_HELP}</span>
        {submit}
      </div>
    </>}
  </form>;
}

/** Puts text on the system clipboard. Input: the text. Output: whether the clipboard took it. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** The text of a terminal's whole buffer, as Copy output takes it. Input: the terminal.
 * Output: the text. */
export function terminalOutput(terminal: Terminal): string {
  const buffer = terminal.buffer.active;
  const lines: string[] = [];
  for (let row = 0; row < buffer.length; row += 1) lines.push(buffer.getLine(row)?.translateToString(true) ?? "");
  return outputText(lines);
}

/** Routes an opened terminal's keys to the agent: what xterm types, Shift+Enter as one new
 * line that xterm never also reads as Enter, the copy key copying a selection instead of
 * typing (off a Mac Ctrl+C then drops the selection, so the next Ctrl+C interrupts the
 * agent), and clipboard pastes with their control characters dropped before xterm brackets
 * them. Inputs: the terminal, the typing action and whether this is a Mac. Output: the
 * subscription for typed text and pastes. */
export function typeKeys(terminal: Terminal, type: (data: string) => void, mac: boolean = isMac()): IDisposable {
  const element = terminal.element;
  if (!element) throw new Error("typeKeys needs an opened terminal");
  const typed = terminal.onData(type);
  terminal.attachCustomKeyEventHandler((event) => {
    if (copyKey(event, terminal.hasSelection(), mac)) {
      if (event.type === "keydown") {
        void copyText(terminal.getSelection());
        if (!mac) terminal.clearSelection();
      }
      return false;
    }
    const override = keyOverride(event);
    if (override.typed !== null) type(override.typed);
    return override.xterm;
  });
  const paste = (event: ClipboardEvent) => {
    event.preventDefault();
    event.stopImmediatePropagation();
    const text = event.clipboardData?.getData("text/plain") ?? "";
    if (text) terminal.paste(pasteText(text));
  };
  element.addEventListener("paste", paste, true);
  return { dispose: () => { typed.dispose(); element.removeEventListener("paste", paste, true); } };
}

/** Routes an opened terminal's input to its send queue while the view is live: typed keys,
 * pastes and mouse reports in order, with the reports xterm makes from a wheel event sent on
 * the wheel lane, so a fast scroll on a full-screen agent is batched. Inputs: the terminal,
 * the send queue, the send call for one chunk, told whether the wheel made it, the live
 * check and whether keys may reach the terminal; without keys only the wheel does. Output:
 * the subscription. */
export function connectInput(terminal: Terminal, run: (job: () => Promise<boolean>) => boolean, send: (chunk: number[], wheel: boolean) => Promise<boolean>, live: () => boolean, keys: () => boolean = () => true): IDisposable {
  const lanes = inputLanes(run, send);
  let wheeling = false;
  terminal.attachCustomWheelEventHandler(() => {
    wheeling = true;
    queueMicrotask(() => { wheeling = false; });
    return true;
  });
  const route = (bytes: Uint8Array) => {
    if (!live()) return;
    if (wheeling) lanes.wheel(bytes);
    else if (keys()) lanes.typed(bytes);
  };
  const typed = typeKeys(terminal, (data) => route(encodeTyped(data)));
  const binary = terminal.onBinary((data) => route(encodeBinary(data)));
  return {
    dispose: () => {
      typed.dispose();
      binary.dispose();
      terminal.attachCustomWheelEventHandler(() => true);
    },
  };
}

/** Tells whether the owner reads above the live screen, so Jump to latest shows only then:
 * once at the start, so a reopened view never keeps the last view's flag, and on every scroll,
 * which xterm also reports when an app switches to or from its full screen.
 * Inputs: the terminal and the setter for that flag. Output: the subscription. */
export function followScroll(terminal: Terminal, set: (behind: boolean) => void): IDisposable {
  const report = () => {
    const buffer = terminal.buffer.active;
    set(buffer.viewportY < buffer.baseY);
  };
  report();
  return terminal.onScroll(report);
}

/** Tells whether the app in a terminal reads the mouse, so the view says how to select text
 * only then: once at the start and after every write xterm parsed, as an app turns mouse
 * reporting on and off with what it writes. Inputs: the terminal and the setter for that
 * flag. Output: the subscription. */
export function followMouse(terminal: Terminal, set: (mouse: boolean) => void): IDisposable {
  const report = () => set(terminal.modes.mouseTrackingMode !== "none");
  report();
  return terminal.onWriteParsed(report);
}

/** Keeps the text the owner selected while the app in a terminal reads the mouse (OB-8,
 * DEC-141): xterm drops a selection whenever it sends the app a mouse report, and an app that
 * asks for every pointer move gets one as soon as the pointer moves, so a forced drag's
 * selection was gone before the copy key or the menu's Copy. While text is selected in such a
 * terminal, a pointer move with no button held and a right-click stay with the view and never
 * reach xterm, so no report is sent for them; a left click still goes to the app and lets the
 * selection go. Inputs: the terminal and the element around its screen. Output: the
 * subscription. */
export function keepSelection(terminal: Terminal, screen: HTMLElement): IDisposable {
  const hold = (event: MouseEvent) => {
    if (terminal.modes.mouseTrackingMode === "none" || !terminal.hasSelection()) return;
    const kept = event.type === "mousemove" ? event.buttons === 0 : event.button === 2;
    if (kept) event.stopPropagation();
  };
  screen.addEventListener("mousemove", hold, true);
  screen.addEventListener("mousedown", hold, true);
  return {
    dispose: () => {
      screen.removeEventListener("mousemove", hold, true);
      screen.removeEventListener("mousedown", hold, true);
    },
  };
}

/** Off a Mac, Shift forces a drag to select while the app reads the mouse, but on the normal
 * screen xterm reads a Shift press as extending the selection from the last click, so a Shift
 * drag with nothing selected selected nothing (G1 N3). Such a press waits for the gesture: a
 * drag hands it to xterm without Shift, starting a new selection at the press, and a release
 * without moving hands it on as it was. Inputs: the terminal, the element around its screen
 * and whether this is a Mac. Output: the subscription. */
export function shiftDragSelects(terminal: Terminal, screen: HTMLElement, mac: boolean = isMac()): IDisposable {
  const page = screen.ownerDocument;
  let held: { target: EventTarget; init: MouseEventInit } | null = null;
  const handOn = (shiftKey: boolean) => {
    if (held === null) return;
    const { target, init } = held;
    held = null;
    target.dispatchEvent(new MouseEvent("mousedown", { ...init, shiftKey }));
  };
  const down = (event: MouseEvent) => {
    held = null;
    if (mac || !event.isTrusted || !event.shiftKey || event.button !== 0 || event.target === null) return;
    if (terminal.modes.mouseTrackingMode !== "none" || terminal.hasSelection()) return;
    event.stopPropagation();
    event.preventDefault();
    held = {
      target: event.target,
      init: { bubbles: true, cancelable: true, composed: true, view: event.view, detail: event.detail, screenX: event.screenX, screenY: event.screenY, clientX: event.clientX, clientY: event.clientY, ctrlKey: event.ctrlKey, altKey: event.altKey, metaKey: event.metaKey, button: event.button, buttons: event.buttons },
    };
  };
  const move = (event: MouseEvent) => {
    if ((event.buttons & 1) !== 0) handOn(false);
  };
  const up = (event: MouseEvent) => {
    if (event.button === 0) handOn(true);
  };
  screen.addEventListener("mousedown", down, true);
  page.addEventListener("mousemove", move, true);
  page.addEventListener("mouseup", up, true);
  return {
    dispose: () => {
      screen.removeEventListener("mousedown", down, true);
      page.removeEventListener("mousemove", move, true);
      page.removeEventListener("mouseup", up, true);
    },
  };
}

/** Where the owner right-clicked a terminal, in page coordinates, and what was selected then. */
export type MenuAt = { x: number; y: number; selection: string };

/** The terminal's right-click menu: Copy for the selection taken when it opened and Copy
 * output for the whole buffer, placed inside its panel near the click, the arrow keys moving
 * between items, Escape and a copy handing focus back to the terminal, and a press outside
 * closing it. Inputs: the terminal, where it opened and the close action, given the note to
 * show after a copy or null. Output: the menu. */
export function TerminalMenu({ terminal, at, onDone }: { terminal: Terminal; at: MenuAt; onDone: (note: string | null) => void }) {
  const list = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const menu = list.current;
    const panel = menu?.offsetParent;
    if (!menu || !panel) return;
    const box = panel.getBoundingClientRect();
    const spot = menuSpot({ x: at.x - box.left - panel.clientLeft, y: at.y - box.top - panel.clientTop },
      { width: menu.offsetWidth, height: menu.offsetHeight }, { width: panel.clientWidth, height: panel.clientHeight });
    menu.style.left = `${spot.x}px`;
    menu.style.top = `${spot.y}px`;
    menu.querySelector<HTMLElement>("[role=menuitem]:not(:disabled)")?.focus();
  }, [at]);
  useEffect(() => {
    const outside = (event: PointerEvent) => { if (!(event.target instanceof Node && list.current?.contains(event.target))) onDone(null); };
    document.addEventListener("pointerdown", outside, true);
    return () => document.removeEventListener("pointerdown", outside, true);
  }, [onDone]);
  const back = (note: string | null) => {
    terminal.focus();
    onDone(note);
  };
  const copy = async (text: string, done: string) => back(await copyText(text) ? done : COPY_REFUSED);
  const move = (step: number) => {
    const items = [...(list.current?.querySelectorAll<HTMLElement>("[role=menuitem]:not(:disabled)") ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    items[(index + step + items.length) % items.length]?.focus();
  };
  return <div ref={list} className="menu-list tg-terminal-menu" role="menu" aria-label="Terminal" onKeyDown={(event) => {
    if (event.key === "Escape") back(null);
    else if (event.key === "ArrowDown") move(1);
    else if (event.key === "ArrowUp") move(-1);
    else return;
    event.preventDefault();
  }}>
    <button type="button" role="menuitem" className="menu-item" disabled={!at.selection} onClick={() => void copy(at.selection, "Copied the selection.")}><Icon name="copy" size={14} />Copy</button>
    <button type="button" role="menuitem" className="menu-item" onClick={() => void copy(terminalOutput(terminal), "Copied the output.")}><Icon name="file-text" size={14} />Copy output</button>
  </div>;
}

/** The head over an agent's terminal, as the approved mockup draws it: the name with its
 * harness and a state badge, Message, which puts the cursor in the box under the terminal,
 * Stop when the agent can be stopped and Close when the view can be closed, then the agent's
 * role, branch, account and harness report, and the swap hint for the one panel beside the
 * graph. A wall tile shows Message and Stop as icons. With no box in the owner's Terminal
 * input there is no Message at all. Inputs: the node, the harness lookup, the tile layout,
 * whether this terminal has a message box, whether it can take a message now, the actions,
 * the report and the hint. Output: the head. */
function TerminalHead({ node, harness, tile, message, boxed, onMessage, onStop, onClose, reported, swaps }: {
  node: GraphNode; harness: (id: string) => string; tile: boolean; message: boolean; boxed: boolean; onMessage: () => void;
  onStop?: () => void; onClose?: () => void; reported?: string; swaps: boolean;
}) {
  const status = nodeStatus(node);
  const label = displayText(node.label);
  const running = nodeRunning(node, Date.now());
  return <div className="tg-terminal-head">
    <span className="tg-terminal-title">
      <Icon name="square-terminal" size={15} />
      <span className="tg-terminal-name">{displayText(`${node.label} · ${harness(node.harness)}`)}</span>
      {status && <StatusBadge word={displayText(status.word)} tone={status.tone} />}
    </span>
    <span className="tg-terminal-actions">
      {message && (tile
        ? <Button variant="quiet" size="small" icon="send" iconOnly={`Message ${label}`} disabled={!boxed} onClick={onMessage} />
        : <Button size="small" icon="send" disabled={!boxed} onClick={onMessage}>Message</Button>)}
      {onStop && (tile
        ? <Button variant="danger-text" size="small" icon="circle-stop" iconOnly={`Stop ${label}`} onClick={onStop} />
        : <Button variant="danger-text" size="small" icon="circle-stop" onClick={onStop}>Stop</Button>)}
      {onClose && <Button variant="quiet" size="small" icon="x" iconOnly="Close terminal" onClick={onClose} />}
    </span>
    <span className="tg-terminal-meta">
      <span className="tg-terminal-fact"><Icon name="user-round" size={12} />{nodeRole(node)}</span>
      {node.branch && <span className="tg-terminal-fact"><Icon name="git-branch" size={12} /><span className="tg-terminal-branch">{displayText(node.branch)}</span></span>}
      {running && <span className="tg-terminal-fact"><Icon name="clock" size={12} />{running}</span>}
      {node.account && <span className="tg-terminal-detail">{displayText(node.account)}</span>}
      {reported && <span className="tg-terminal-detail" title={displayText(reported)}>{displayText(reported)}</span>}
      {swaps && <span className="tg-terminal-fact tg-terminal-hint"><Icon name="info" size={12} />Click another agent to show it here</span>}
    </span>
  </div>;
}

let parked: HTMLDivElement | null = null;

/** The box hidden terminal views wait in (DEC-187): laid out on screen at the size each had
 * in its panel, so xterm keeps drawing it and showing it again needs no fit, but invisible,
 * inert and hidden from assistive technology, so it never takes a click or the focus.
 * Output: the box, made on first use. */
function parking(): HTMLDivElement {
  if (parked?.isConnected) return parked;
  const box = document.createElement("div");
  box.className = "tg-terminal-parking";
  box.inert = true;
  box.setAttribute("aria-hidden", "true");
  document.body.appendChild(box);
  parked = box;
  return box;
}

const views = new Set<KeptView>();
let themed = false;
let rendered = false;

/** Repaints every open terminal view in the app's colors once the owner's appearance or the
 * system's changes, since a kept view outlives the panel that read them. Output: none. */
function followTheme(): void {
  if (themed) return;
  themed = true;
  const apply = () => {
    const theme = terminalOptions().theme;
    for (const view of views) if (view.terminal) view.terminal.options.theme = theme;
  };
  new MutationObserver(apply).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", apply);
}

/** Follows the owner's Terminal renderer choice once for the window: a new choice forgets a
 * failed Automatic start and draws every view afresh with it, so open terminals change at
 * once. Output: none. */
function followRenderer(): void {
  if (rendered) return;
  rendered = true;
  onTerminalRenderer(() => {
    rendererChosen();
    for (const view of views) view.redraw();
  });
}

/** One agent terminal view that outlives its panel (DEC-187): an xterm made once in an element
 * of its own and attached once. Showing moves that element into a panel's screen and hiding
 * moves it to the parking box, so neither rebuilds the screen nor calls the bridge. While
 * hidden, output is written in one batch per `HIDDEN_FLUSH_MS`. The view fits and sends its
 * size only once it knows its panel's size, from a show or the size its panel last had, so a
 * view loaded before any panel never sizes the agent's terminal. Follow and Open again connect
 * again in place. A wall tile makes one per tile and drops it with the tile. Inputs: the key
 * and the size its panel last had, if known. */
export class KeptView implements Held {
  readonly key: KeptKey;
  readonly id: string;
  readonly host: HTMLDivElement;
  shown = false;
  follow = false;
  keys = true;
  terminal: Terminal | null = null;
  prompt: ((text: string) => boolean) | null = null;
  private state: ViewState = IDLE;
  private readonly heard = new Set<() => void>();
  private readonly hidden = new HiddenOutput();
  private link: Link | null = null;
  private fit: FitAddon | null = null;
  private Webgl: (new () => WebglAddon) | null = null;
  private gpu: GpuDrawing | null = null;
  private gpuLost = false;
  private size: { width: number; height: number } | null = null;
  private fitted = false;
  private tries = 0;
  private dropped = false;
  private flushTimer = 0;
  private againTimer = 0;
  private resizeTimer = 0;
  private fitFrame = 0;
  private focusFrame = 0;
  private release = () => {};

  constructor(key: KeptKey, size: { width: number; height: number } | null = null) {
    assert(size === null || (size.width > 0 && size.height > 0), "a parked size is never empty");
    this.key = key;
    this.id = keyId(key);
    this.size = size;
    this.host = document.createElement("div");
    this.host.className = "tg-terminal-host";
    views.add(this);
    followTheme();
    followRenderer();
    this.park();
    void this.open();
  }

  /** Whether the view was let go. Output: the answer. */
  get gone(): boolean {
    return this.dropped;
  }

  /** Hears every change of what the view shows, for `useSyncExternalStore`. Input: the
   * listener. Output: the call that stops it. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.heard.add(listener);
    return () => {
      this.heard.delete(listener);
    };
  };

  /** What the view shows now: its state, whether the owner reads above the live screen and
   * whether the app reads the mouse. Output: the same object until one of them changes. */
  readonly snapshot = (): ViewState => this.state;

  /** The size in px the view's panel last had, if known. Output: the size, or null. */
  get parked(): { width: number; height: number } | null {
    return this.size;
  }

  /** Shows the view in a panel's screen: writes the output held while hidden in one write,
   * then moves its element in. Nothing here waits or measures: no bridge call and no fit
   * before the frame. The element's size watch fits after the frame only when the screen's
   * size differs from the parked one. A view whose terminal ended connects again once it is
   * in, as a panel opened anew would. After the frame a live view takes the focus and, once
   * fitted, sends its size once, since another window may have resized the terminal while it
   * was hidden. Input: the screen element. Output: none. */
  show(screen: HTMLElement): void {
    assert(!this.dropped, "a view let go is never shown");
    this.shown = true;
    this.flush();
    this.host.style.width = "";
    this.host.style.height = "";
    screen.appendChild(this.host);
    this.gpuLost = false;
    this.draw();
    if (this.state.view.kind === "ended") this.reopen();
    window.cancelAnimationFrame(this.focusFrame);
    this.focusFrame = window.requestAnimationFrame(() => {
      if (!this.shown || !this.link?.live) return;
      this.terminal?.focus();
      if (this.fitted) this.sendSize();
    });
  }

  /** Hides the view: moves its element to the parking box at the size it had, keeping its
   * screen, scrollback and connection. Output: none. */
  hide(): void {
    this.shown = false;
    window.cancelAnimationFrame(this.focusFrame);
    this.releaseGpu();
    if (!this.dropped) this.park();
  }

  /** Draws the view afresh under a new Terminal renderer choice, a lost GPU context
   * forgotten. Output: none. */
  redraw(): void {
    this.gpuLost = false;
    this.draw();
  }

  /** Scrolls to the latest output and puts the cursor in the terminal. Output: none. */
  latest(): void {
    this.terminal?.scrollToBottom();
    this.terminal?.focus();
  }

  /** Connects again after the owner's Open again, with the follow count started afresh.
   * Output: none. */
  reopen(): void {
    this.tries = 0;
    this.reconnect();
  }

  /** Lets the view go: ends its connection with a detach, frees its terminal and removes its
   * element. The agent keeps running. Output: none. */
  drop(): void {
    if (this.dropped) return;
    this.dropped = true;
    this.shown = false;
    views.delete(this);
    window.clearTimeout(this.flushTimer);
    window.clearTimeout(this.againTimer);
    window.clearTimeout(this.resizeTimer);
    window.cancelAnimationFrame(this.fitFrame);
    window.cancelAnimationFrame(this.focusFrame);
    this.link?.release();
    this.link = null;
    this.hidden.take();
    this.releaseGpu();
    this.release();
    this.terminal = null;
    this.fit = null;
    this.host.remove();
    this.heard.clear();
  }

  private set(next: Partial<ViewState>): void {
    const state = { ...this.state, ...next };
    if (state.view === this.state.view && state.behind === this.state.behind && state.mouse === this.state.mouse) return;
    this.state = state;
    for (const listener of [...this.heard]) listener();
  }

  private park(): void {
    if (this.size) {
      this.host.style.width = `${this.size.width}px`;
      this.host.style.height = `${this.size.height}px`;
    }
    parking().appendChild(this.host);
  }

  private async open(): Promise<void> {
    const [{ Terminal }, { FitAddon }, webgl] = await Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
      import("@xterm/addon-webgl").catch(() => null),
      import("@xterm/xterm/css/xterm.css"),
    ]);
    if (this.dropped) return;
    const terminal = new Terminal(terminalOptions());
    muteQueries(terminal);
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(this.host);
    readableScreen(this.host, terminal);
    this.terminal = terminal;
    this.fit = fit;
    this.Webgl = webgl?.WebglAddon ?? null;
    this.draw();
    const scrolled = followScroll(terminal, (behind) => this.set({ behind }));
    const moused = followMouse(terminal, (mouse) => this.set({ mouse }));
    const selected = keepSelection(terminal, this.host);
    const shifted = shiftDragSelects(terminal, this.host);
    const resized = terminal.onResize(() => {
      window.clearTimeout(this.resizeTimer);
      this.resizeTimer = window.setTimeout(() => this.sendSize(), RESIZE_PAUSE_MS);
    });
    const observer = new ResizeObserver(() => {
      const width = this.host.offsetWidth;
      const height = this.host.offsetHeight;
      if (this.shown && width > 0 && height > 0 && (width !== this.size?.width || height !== this.size?.height)) {
        this.size = { width, height };
        if (kept.get(this.key) === this) rememberSize(this);
      }
      this.refit();
    });
    observer.observe(this.host);
    this.release = () => {
      observer.disconnect();
      resized.dispose();
      scrolled.dispose();
      moused.dispose();
      selected.dispose();
      shifted.dispose();
      terminal.dispose();
    };
    this.connect();
  }

  /** Draws the view on the GPU while it is shown and the owner's Terminal renderer choice
   * wants the GPU on this machine, and with DOM rows otherwise, so a hidden view holds no GPU
   * context. A start the GPU refuses counts against Automatic; a lost context keeps DOM rows
   * until the view is shown again or the choice changes. A new drawing repaints the screen.
   * Output: none. */
  private draw(): void {
    const terminal = this.terminal;
    if (!terminal || this.dropped) return;
    const choice = readTerminalRenderer();
    if (!this.shown || this.gpuLost || this.Webgl === null || !gpuWanted(choice, probeGpu, autoStartFailed())) {
      this.releaseGpu();
      return;
    }
    if (this.gpu) return;
    const drawing = drawOnGpu(terminal, this.Webgl, () => {
      if (this.gpu !== drawing) return;
      this.gpu = null;
      this.gpuLost = true;
    });
    if (!drawing) {
      gpuStartFailed(choice);
      return;
    }
    this.gpu = drawing;
    terminal.refresh(0, terminal.rows - 1);
  }

  /** Hands the view's drawing back to DOM rows and frees its GPU context. Output: none. */
  private releaseGpu(): void {
    const drawing = this.gpu;
    this.gpu = null;
    drawing?.release();
  }

  private refit(): void {
    window.cancelAnimationFrame(this.fitFrame);
    this.fitFrame = window.requestAnimationFrame(() => {
      if (this.fitted && !this.dropped && this.size && this.host.offsetWidth > 0 && this.host.offsetHeight > 0) this.fit?.fit();
    });
  }

  private sendSize(): void {
    const terminal = this.terminal;
    if (!terminal || !this.link?.live || !this.size) return;
    const size = terminalSize(terminal.cols, terminal.rows);
    const { terminal: name, pane } = this.key;
    if (size) void resizeTerminal(name, size.cols, size.rows, pane);
  }

  private output(bytes: Uint8Array): void {
    const terminal = this.terminal;
    const link = this.link;
    if (!terminal || !link) return;
    if (this.shown) {
      terminal.write(bytes, link.wrote());
      return;
    }
    if (this.hidden.push(bytes)) {
      this.flush();
      return;
    }
    if (this.flushTimer === 0) this.flushTimer = window.setTimeout(() => this.flush(), HIDDEN_FLUSH_MS);
  }

  private flush(): void {
    window.clearTimeout(this.flushTimer);
    this.flushTimer = 0;
    const bytes = this.hidden.take();
    if (bytes && this.terminal && this.link) this.terminal.write(bytes, this.link.wrote());
  }

  private end(link: Link, copy: string): void {
    link.live = false;
    if (link.closed || this.dropped) return;
    const next = this.follow ? followTry(this.tries, link.openedAt, performance.now()) : null;
    if (next !== null) {
      this.tries = next;
      this.hidden.take();
      this.terminal?.reset();
      this.set({ view: { kind: "connecting", reason: copy, tries: next } });
      window.clearTimeout(this.againTimer);
      this.againTimer = window.setTimeout(() => this.reconnect(), FOLLOW_MS);
      return;
    }
    this.set({ view: { kind: "ended", copy } });
  }

  private reconnect(): void {
    window.clearTimeout(this.againTimer);
    if (this.dropped || !this.terminal) return;
    this.link?.release();
    this.link = null;
    this.hidden.take();
    window.clearTimeout(this.flushTimer);
    this.flushTimer = 0;
    this.fitted = false;
    this.terminal.reset();
    if (this.state.view.kind !== "connecting") this.set({ view: { kind: "connecting" } });
    this.connect();
  }

  private connect(): void {
    const terminal = this.terminal;
    assert(terminal !== null && this.link === null, "a view connects with its terminal open and no link held");
    const { terminal: name, pane, project } = this.key;
    let unsettle = () => {};
    const link: Link = { live: false, openedAt: null, serial: null, closed: false, wrote: () => () => {}, release: () => {} };
    link.wrote = settleAfterWrites((run) => {
      const id = window.requestAnimationFrame(run);
      unsettle = () => window.cancelAnimationFrame(id);
      return unsettle;
    }, () => {
      if (link.closed || this.dropped) return;
      this.fitted = true;
      if (!this.size) return;
      this.fit?.fit();
      this.sendSize();
    });
    const run = jobQueue(() => this.end(link, STOPPED_TYPING));
    const typing = connectInput(terminal, run, async (chunk, wheel) => (await typeIntoTerminal(name, chunk, wheel, pane)).kind === "ok", () => link.live, () => this.keys);
    const prompt = (text: string) => {
      if (!link.live || !run(async () => (await promptTerminal(name, text, pane)).kind === "ok")) return false;
      terminal.focus();
      return true;
    };
    this.prompt = prompt;
    const channel = new Channel<TerminalMessage>((message) => {
      if (link.closed) return;
      const event = terminalEvent(message);
      if (event.kind === "output") {
        this.output(event.bytes);
      } else if (event.kind === "attached") {
        const size = terminalSize(event.cols, event.rows);
        if (size) terminal.resize(size.cols, size.rows);
        link.openedAt = performance.now();
        link.live = true;
        this.set({ view: { kind: "live" } });
        if (this.shown) terminal.focus();
      } else {
        this.end(link, event.copy);
      }
    });
    link.release = () => {
      link.closed = true;
      link.live = false;
      if (this.prompt === prompt) this.prompt = null;
      channel.onmessage = () => {};
      typing.dispose();
      unsettle();
      if (link.serial !== null) void detachTerminal(name, link.serial);
    };
    this.link = link;
    void attachTerminal(name, channel, pane, project).then((result) => {
      if (link.closed) {
        if (result.kind === "ok") void detachTerminal(name, result.value);
        return;
      }
      if (result.kind === "ok") link.serial = result.value;
      else if (result.kind === "not_ready") this.set({ view: { kind: "not_ready" } });
      else if (result.kind === "error") this.end(link, result.code.startsWith("terminal_") ? endCopy(result.code) : result.message);
    });
  }
}

const kept = new KeptSet<KeptView>(KEPT_MAX);

function letGo(dropped: KeptView[]): void {
  for (const view of dropped) view.drop();
}

/** The main window's kept view for a key, marked most recently used, or a new one made,
 * attached and kept, which drops the least recently used hidden view past `KEPT_MAX`.
 * Input: the key. Output: the view. */
function keptView(key: KeptKey): KeptView {
  const found = kept.get(key);
  const view = found ?? new KeptView(key);
  if (found) kept.touch(key);
  else letGo(kept.add(view));
  rememberShown(view);
  return view;
}

function savedOf(view: KeptView): SavedView | null {
  const { project, terminal, pane } = view.key;
  return project === null ? null : { key: { project, terminal, pane }, size: view.parked };
}

/** Remembers a view the owner shows as the most recently used, for the next app start. Input:
 * the view. Output: none. */
function rememberShown(view: KeptView): void {
  const entry = savedOf(view);
  if (entry) writeSaved(shownFirst(readSaved(), entry));
}

/** Remembers the size a remembered view's panel has now, in the same order. Input: the view.
 * Output: none. */
function rememberSize(view: KeptView): void {
  const entry = savedOf(view);
  if (entry?.size) writeSaved(resized(readSaved(), entry.key, entry.size));
}

/** Lets go of the kept terminal views whose project is no longer open: one removed or closed,
 * or every view when the session starts again (DEC-187: nothing kept after sign-out or project
 * close). Input: whether a project stays open. Output: none. */
export function keepTerminalsOf(open: (project: string | null) => boolean): void {
  letGo(kept.dropWhere((key) => !open(key.project)));
}

let epoch = 0;

/** Loads a remembered terminal hidden, behind the views the owner opened since, when the set
 * has room and the project's team still lists that terminal. Inputs: the entry, the session it
 * was read in, and the project's team graph when the caller already read it, so app start
 * reads each graph once; with none given it is read here. Output: none. */
async function preloadView(entry: SavedView, at: number, read: Result<TeamGraph> | null = null): Promise<void> {
  assert(read === null || read.kind === "ok", "a team graph passed in is one that was read");
  const graph = read ?? await loadTeamGraph(entry.key.project);
  if (at !== epoch || graph.kind !== "ok" || !kept.roomBehind(entry.key)) return;
  if (!graph.value.nodes.some((node) => node.terminal === entry.key.terminal && teammatePane(node) === entry.key.pane)) return;
  kept.keepBehind(new KeptView(entry.key, entry.size));
}

/** Loads the terminals the owner used last, most recent first, hidden and unfocused; with none
 * remembered, the master of the project whose page was opened last. Only listed projects
 * count and nothing is started: a terminal the team does not list is skipped. Inputs: the
 * listed projects and the session. Output: none. */
async function preloadKept(listed: ReadonlySet<string>, at: number): Promise<void> {
  const saved = readSaved().filter((entry) => listed.has(entry.key.project));
  for (const entry of saved) await preloadView(entry, at);
  const last = readLastProject();
  if (saved.length > 0 || last === null || !listed.has(last)) return;
  const graph = await loadTeamGraph(last);
  const master = graph.kind === "ok" ? graph.value.nodes.find((node) => node.kind === "master") : undefined;
  if (at === epoch && master?.terminal) await preloadView({ key: { project: last, terminal: master.terminal, pane: teammatePane(master) }, size: null }, at, graph);
}

/** Keeps the main window's kept terminal views to open projects (DEC-187): once the project
 * list reads complete, the views of a project it no longer lists go and are forgotten, and
 * when the session starts again, as after an engine reconnect or a profile change, every view
 * goes. A list with more projects than it holds drops nothing. Once per session, after the
 * window has drawn, the terminals used last load in the background. Inputs: the session's
 * projects and whether more are unlisted. Output: none. */
export function useKeptTerminalProjects(projects: Loaded<ProjectRow[]>, more: boolean): void {
  const preloaded = useRef(false);
  useEffect(() => {
    if (projects.kind === "loading") {
      epoch += 1;
      preloaded.current = false;
      keepTerminalsOf(() => false);
      return;
    }
    if (projects.kind !== "ok") return;
    const listed = new Set(projects.value.map((project) => project.id));
    if (!more) {
      keepTerminalsOf((project) => project !== null && listed.has(project));
      const saved = readSaved();
      const open = saved.filter((entry) => listed.has(entry.key.project));
      if (open.length !== saved.length) writeSaved(open);
    }
    if (preloaded.current) return;
    preloaded.current = true;
    const at = epoch;
    afterFirstPaint(() => void preloadKept(listed, at));
  }, [projects, more]);
}

const still = () => () => {};
const idle = () => IDLE;

/** One agent's live terminal beside the graph: the screen at attach, live output, and every
 * key typed straight into the agent's real terminal, sized to the panel, with a way back to
 * the latest output once scrolled up, and Copy and Copy output on right-click and in its
 * status line, which also says how to select text while the app in the terminal reads the
 * mouse; that selection stays until the owner copies it or clicks. In the main window the
 * view stays loaded and attached after the panel closes, among the two most recently used
 * (DEC-187), so showing it again only moves it back; a wall tile's view detaches with the
 * tile. The agent keeps running either way.
 * Inputs: the graph node, a harness name lookup, the close action when the panel can be
 * closed, the stop action when the agent can be stopped, `embedded` when a console around
 * it owns the head, `tile` for a terminal wall tile, `swaps` for the one panel a graph click
 * replaces, `follow` when an ended terminal opens again on its own, cleared, for an agent
 * that restarts in a new pane, and the one-line report of what the agent's harness loaded and
 * invoked, shown in the head, the project the view is kept under and a wall window attaches
 * for, and what hears the
 * terminal open at its first frame and close, so the live master's wait ends only then. Where the owner
 * types (the terminal, the message box under it, or both) is the one Terminal input in
 * Settings, for every agent on every screen. Output: the panel. */
export function TerminalPanel({ node, harness = (id) => id, onClose, onStop, embedded = false, tile = false, swaps = false, follow = false, reported, projectId, onOpened, seed = null }: {
  node: GraphNode;
  harness?: (id: string) => string;
  onClose?: () => void;
  onStop?: () => void;
  embedded?: boolean;
  tile?: boolean;
  swaps?: boolean;
  follow?: boolean;
  reported?: string;
  projectId?: string;
  onOpened?: (open: boolean) => void;
  seed?: Seed | null;
}) {
  if (embedded && tile) throw new Error("a terminal is either embedded in a console or a wall tile, never both");
  if (embedded && swaps) throw new Error("only a terminal with its own head says it swaps");
  const input = readTerminalInput();
  const mode = inputMode(input);
  const box = useRef<HTMLTextAreaElement>(null);
  const following = useRef(follow);
  following.current = follow;
  const keys = useRef(mode.keys);
  keys.current = mode.keys;
  const screen = useRef<HTMLDivElement>(null);
  const name = node.terminal;
  const pane = teammatePane(node);
  const key: KeptKey | null = name ? { project: projectId ?? null, terminal: name, pane } : null;
  const id = key ? keyId(key) : null;
  const [made, setMade] = useState<KeptView | null>(null);
  const found = key && !tile ? kept.get(key) ?? null : null;
  const entry = found ?? (made && made.id === id && !made.gone ? made : null);
  const current = useRef(entry);
  current.current = entry;
  const state = useSyncExternalStore(entry?.subscribe ?? still, entry?.snapshot ?? idle, entry?.snapshot ?? idle);
  const view = state.view;
  const opened = useRef(onOpened);
  opened.current = onOpened;
  const open = view.kind === "live";
  useEffect(() => { opened.current?.(open); }, [open]);
  useEffect(() => () => opened.current?.(false), []);
  const pasted = useRef<number | null>(null);
  useEffect(() => {
    const terminal = current.current?.terminal ?? null;
    if (seed === null || !open || terminal === null || pasted.current === seed.id) return;
    pasted.current = seed.id;
    terminal.paste(seedText(seed.text, terminal.modes.bracketedPasteMode));
    terminal.focus();
    seed.used();
  }, [seed, open]);
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(null), COPIED_MS);
    return () => window.clearTimeout(timer);
  }, [copied]);

  useEffect(() => {
    if (view.kind === "connecting") setMenu(null);
  }, [view.kind]);

  const copy = async (text: string, done: string) => setCopied(await copyText(text) ? done : COPY_REFUSED);
  const closeMenu = useRef((note: string | null) => {
    setMenu(null);
    if (note) setCopied(note);
  }).current;

  useLayoutEffect(() => {
    if (!entry) return;
    entry.follow = following.current;
    entry.keys = keys.current;
  });

  useLayoutEffect(() => {
    const element = screen.current;
    if (!key || !element) return;
    setMenu(null);
    const shown = tile ? new KeptView(key) : keptView(key);
    shown.follow = following.current;
    shown.keys = keys.current;
    shown.show(element);
    if (shown !== current.current) setMade(shown);
    return () => {
      shown.hide();
      if (tile) shown.drop();
      else letGo(kept.trim());
    };
  }, [id, tile]);

  const boxed = mode.box && Boolean(name) && view.kind !== "not_ready";
  const terminal = entry?.terminal ?? null;
  return <section className={embedded ? "tg-terminal embedded" : tile ? "tg-terminal wall-tile" : "tg-terminal"} aria-label={`${node.label} terminal`}>
    {!embedded && <TerminalHead node={node} harness={harness} tile={tile} message={mode.box} boxed={boxed} onMessage={() => box.current?.focus()}
      onStop={onStop} onClose={onClose} reported={reported} swaps={swaps} />}
    {!name && <p className="tg-terminal-state" role="status">This agent has no terminal yet. It opens once the agent starts.</p>}
    {view.kind === "not_ready" && <NotReady what="Agent terminals" compact />}
    {view.kind === "connecting" && name && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />{displayText(connectingLine(view.reason ?? null, view.tries ?? 0))}</p>}
    {view.kind === "ended" && <div className="tg-terminal-state" role="alert">
      <span>{view.copy}</span>
      <Button size="small" icon="refresh-cw" onClick={() => entry?.reopen()}>Open again</Button>
    </div>}
    <div className="tg-terminal-screen" ref={screen} hidden={!name || view.kind === "not_ready"} onContextMenu={(event) => {
      const clicked = entry?.terminal;
      if (!clicked) return;
      event.preventDefault();
      setMenu({ x: event.clientX, y: event.clientY, selection: clicked.getSelection() });
    }} />
    {menu && terminal && <TerminalMenu terminal={terminal} at={menu} onDone={closeMenu} />}
    {view.kind === "live" && <div className="tg-terminal-note">
      <span role="status">{copied ?? liveNote(node, input, state.mouse)}</span>
      {state.behind && <Button variant="quiet" size="small" icon="chevron-down" onClick={() => entry?.latest()}>Jump to latest</Button>}
      <Button variant="quiet" size="small" icon="copy" onClick={() => void copy(terminal ? terminalOutput(terminal) : "", "Copied the output.")}>Copy output</Button>
    </div>}
    {boxed && <Composer node={node} live={view.kind === "live"} onSend={(text) => entry?.prompt?.(text) ?? false} compact={!embedded} box={box} />}
  </section>;
}
