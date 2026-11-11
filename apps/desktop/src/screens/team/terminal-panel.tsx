import { Channel } from "@tauri-apps/api/core";
import type { IDisposable, ITerminalOptions, Terminal } from "@xterm/xterm";
import { useEffect, useLayoutEffect, useRef, useState, type Ref } from "react";
import { attachTerminal, detachTerminal, promptTerminal, resizeTerminal, typeIntoTerminal } from "../../api";
import type { GraphNode, TerminalMessage } from "../../engine-client";
import type { InputChoice } from "./terminal-io";
import { readTerminalInput } from "../../state";
import { Button, Icon, NotReady, StatusBadge, isMac, sendsMessage } from "../../ui";
import { displayText } from "../conversation";
import { nodeRole, nodeRunning, nodeStatus } from "./graph";
import { copyKey, encodeBinary, encodeTyped, endCopy, inputLanes, inputMode, jobQueue, keyOverride, menuSpot, muteQueries, outputText, pasteText, promptText, settleAfterWrites, terminalEvent, terminalSize } from "./terminal-io";

const COPIED_MS = 2000;
const COPY_REFUSED = "The clipboard refused the copy.";
const FOLLOW_MS = 1000;
const FOLLOW_TRIES = 30;
const RESIZE_PAUSE_MS = 120;
const SCROLLBACK_LINES = 5000;
const STOPPED_TYPING = "Typing stopped reaching this agent. Open the terminal again to reconnect.";

type View = { kind: "connecting" } | { kind: "live" } | { kind: "ended"; copy: string } | { kind: "not_ready" };

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

/** One agent's live terminal beside the graph: the screen at attach, live output, and every
 * key typed straight into the agent's real terminal, sized to the panel, with a way back to
 * the latest output once scrolled up, and Copy and Copy output on right-click and in its
 * status line. Closing it detaches the view; the agent keeps running.
 * Inputs: the graph node, a harness name lookup, the close action when the panel can be
 * closed, the stop action when the agent can be stopped, `embedded` when a console around
 * it owns the head, `tile` for a terminal wall tile, `swaps` for the one panel a graph click
 * replaces, `follow` when an ended terminal opens again on its own, cleared, for an agent
 * that restarts in a new pane, and the one-line report of what the agent's harness loaded and
 * invoked, shown in the head, and the project id a wall window attaches for. Where the owner
 * types (the terminal, the message box under it, or both) is the one Terminal input in
 * Settings, for every agent on every screen. Output: the panel. */
export function TerminalPanel({ node, harness = (id) => id, onClose, onStop, embedded = false, tile = false, swaps = false, follow = false, reported, projectId }: {
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
}) {
  if (embedded && tile) throw new Error("a terminal is either embedded in a console or a wall tile, never both");
  if (embedded && swaps) throw new Error("only a terminal with its own head says it swaps");
  const input = readTerminalInput();
  const mode = inputMode(input);
  const box = useRef<HTMLTextAreaElement>(null);
  const following = useRef(follow);
  following.current = follow;
  const tries = useRef(0);
  const keys = useRef(mode.keys);
  keys.current = mode.keys;
  const screen = useRef<HTMLDivElement>(null);
  const prompt = useRef<((text: string) => boolean) | null>(null);
  const latest = useRef<(() => void) | null>(null);
  const term = useRef<Terminal | null>(null);
  const [view, setView] = useState<View>({ kind: "connecting" });
  const [behind, setBehind] = useState(false);
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(null), COPIED_MS);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const copy = async (text: string, done: string) => setCopied(await copyText(text) ? done : COPY_REFUSED);
  const closeMenu = useRef((note: string | null) => {
    setMenu(null);
    if (note) setCopied(note);
  }).current;
  const [round, setRound] = useState(0);
  const name = node.terminal;
  const pane = teammatePane(node);

  useEffect(() => {
    const element = screen.current;
    if (!name || !element) return;
    setView({ kind: "connecting" });
    setMenu(null);
    let disposed = false;
    let serial: number | null = null;
    let again = 0;
    let release = () => {};
    void (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import("@xterm/xterm"),
        import("@xterm/addon-fit"),
        import("@xterm/xterm/css/xterm.css"),
      ]);
      if (disposed) return;
      const terminal = new Terminal(terminalOptions());
      muteQueries(terminal);
      const fit = new FitAddon();
      terminal.loadAddon(fit);
      terminal.open(element);
      let live = false;
      let fitted = false;
      let timer = 0;
      let frame = 0;
      const end = (copy: string) => {
        live = false;
        if (disposed) return;
        if (following.current && tries.current < FOLLOW_TRIES) {
          tries.current += 1;
          terminal.reset();
          setView({ kind: "connecting" });
          window.clearTimeout(again);
          again = window.setTimeout(() => setRound((value) => value + 1), FOLLOW_MS);
          return;
        }
        setView({ kind: "ended", copy });
      };
      const sendSize = () => {
        const size = terminalSize(terminal.cols, terminal.rows);
        if (live && size) void resizeTerminal(name, size.cols, size.rows);
      };
      const refit = () => {
        window.cancelAnimationFrame(frame);
        frame = window.requestAnimationFrame(() => { if (fitted && !disposed) fit.fit(); });
      };
      let unsettle = () => {};
      const wrote = settleAfterWrites((run) => {
        const id = window.requestAnimationFrame(run);
        unsettle = () => window.cancelAnimationFrame(id);
        return unsettle;
      }, () => {
        if (disposed) return;
        fitted = true;
        fit.fit();
        sendSize();
      });
      const run = jobQueue(() => end(STOPPED_TYPING));
      const typing = connectInput(terminal, run, async (chunk, wheel) => (await typeIntoTerminal(name, chunk, wheel)).kind === "ok", () => live, () => keys.current);
      prompt.current = (text) => {
        if (!live || !run(async () => (await promptTerminal(name, text)).kind === "ok")) return false;
        terminal.focus();
        return true;
      };
      const scrolled = followScroll(terminal, setBehind);
      latest.current = () => {
        terminal.scrollToBottom();
        terminal.focus();
      };
      term.current = terminal;
      const resized = terminal.onResize(() => {
        window.clearTimeout(timer);
        timer = window.setTimeout(sendSize, RESIZE_PAUSE_MS);
      });
      const observer = new ResizeObserver(refit);
      observer.observe(element);
      const channel = new Channel<TerminalMessage>((message) => {
        if (disposed) return;
        const event = terminalEvent(message);
        if (event.kind === "output") {
          terminal.write(event.bytes, wrote());
        } else if (event.kind === "attached") {
          const size = terminalSize(event.cols, event.rows);
          if (size) terminal.resize(size.cols, size.rows);
          tries.current = 0;
          live = true;
          setView({ kind: "live" });
          terminal.focus();
        } else {
          end(event.copy);
        }
      });
      release = () => {
        prompt.current = null;
        latest.current = null;
        term.current = null;
        channel.onmessage = () => {};
        observer.disconnect();
        typing.dispose();
        resized.dispose();
        scrolled.dispose();
        window.clearTimeout(timer);
        window.cancelAnimationFrame(frame);
        unsettle();
        terminal.dispose();
      };
      const result = await attachTerminal(name, channel, pane, projectId ?? null);
      if (disposed) {
        if (result.kind === "ok") void detachTerminal(name, result.value);
        return;
      }
      if (result.kind === "ok") serial = result.value;
      else if (result.kind === "not_ready") setView({ kind: "not_ready" });
      else if (result.kind === "error") end(result.code.startsWith("terminal_") ? endCopy(result.code) : result.message);
    })();
    return () => {
      disposed = true;
      window.clearTimeout(again);
      release();
      if (serial !== null) void detachTerminal(name, serial);
    };
  }, [name, pane, projectId, round]);

  const boxed = mode.box && Boolean(name) && view.kind !== "not_ready";
  return <section className={embedded ? "tg-terminal embedded" : tile ? "tg-terminal wall-tile" : "tg-terminal"} aria-label={`${node.label} terminal`}>
    {!embedded && <TerminalHead node={node} harness={harness} tile={tile} message={mode.box} boxed={boxed} onMessage={() => box.current?.focus()}
      onStop={onStop} onClose={onClose} reported={reported} swaps={swaps} />}
    {!name && <p className="tg-terminal-state" role="status">This agent has no terminal yet. It opens once the agent starts.</p>}
    {view.kind === "not_ready" && <NotReady what="Agent terminals" compact />}
    {view.kind === "connecting" && name && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Connecting to the terminal...</p>}
    {view.kind === "ended" && <div className="tg-terminal-state" role="alert">
      <span>{view.copy}</span>
      <Button size="small" icon="refresh-cw" onClick={() => { tries.current = 0; setRound((value) => value + 1); }}>Open again</Button>
    </div>}
    <div className="tg-terminal-screen" ref={screen} hidden={!name || view.kind === "not_ready"} onContextMenu={(event) => {
      if (!term.current) return;
      event.preventDefault();
      setMenu({ x: event.clientX, y: event.clientY, selection: term.current.getSelection() });
    }} />
    {menu && term.current && <TerminalMenu terminal={term.current} at={menu} onDone={closeMenu} />}
    {view.kind === "live" && <div className="tg-terminal-note">
      <span role="status">{copied ?? inputNote(node, input)}</span>
      {behind && <Button variant="quiet" size="small" icon="chevron-down" onClick={() => latest.current?.()}>Jump to latest</Button>}
      <Button variant="quiet" size="small" icon="copy" onClick={() => void copy(term.current ? terminalOutput(term.current) : "", "Copied the output.")}>Copy output</Button>
    </div>}
    {boxed && <Composer node={node} live={view.kind === "live"} onSend={(text) => prompt.current?.(text) ?? false} compact={!embedded} box={box} />}
  </section>;
}
