import { Channel } from "@tauri-apps/api/core";
import type { IDisposable, ITerminalOptions, Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";
import { attachTerminal, detachTerminal, promptTerminal, resizeTerminal, typeIntoTerminal } from "../../api";
import type { GraphNode, TerminalMessage } from "../../engine-client";
import type { InputChoice } from "./terminal-io";
import { Button, Icon, NotReady, isMac, sendsMessage } from "../../ui";
import { displayText } from "../conversation";
import { nodeLine } from "./graph";
import { copyKey, encodeBinary, encodeTyped, endCopy, inputLanes, inputMode, jobQueue, keyOverride, muteQueries, outputText, pasteText, promptText, settleAfterWrites, terminalEvent, terminalSize } from "./terminal-io";

const COPIED_MS = 2000;
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
 * reads the mouse, and `SCROLLBACK_LINES` of scrollback for the history the engine sends on
 * attach. Output: the options for a new terminal. */
export function terminalOptions(): ITerminalOptions {
  return {
    cursorBlink: true,
    fontFamily: token("--mono", "ui-monospace, Menlo, monospace"),
    fontSize: 13,
    macOptionIsMeta: true,
    macOptionClickForcesSelection: true,
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

/** The note under a live terminal: who reads what the owner types. Input: the node.
 * Output: one sentence. */
export function typingNote(node: GraphNode): string {
  return node.kind === "master"
    ? "Typing goes straight to the master."
    : "Typing goes straight to this agent and is not kept. Only a prompt you send below is also told to its master.";
}

/** The status line's note on where the owner's typing goes. Inputs: the node and the input
 * choice. Output: one sentence. */
export function inputNote(node: GraphNode, input: InputChoice): string {
  if (input === "box") return `The terminal shows ${node.label}; type in the message box below.`;
  if (input === "terminal") return `${typingNote(node)} Enter sends, Shift+Enter adds a line.`;
  return typingNote(node);
}

/** The composer's label and placeholder for one agent. Input: the node. Output: both texts. */
export function promptCopy(node: GraphNode): { label: string; placeholder: string } {
  return node.kind === "master"
    ? { label: "Prompt the master", placeholder: "Tell the master what to do next" }
    : { label: `Prompt ${node.label}`, placeholder: `Tell ${node.label} what to do. Its master is told what you send.` };
}

/** The prompt box under an agent's terminal: Enter sends the text as one message, Shift+Enter
 * adds a line. The box always takes typing and keeps its draft, as a master turn's terminal
 * ends after each turn; only sending waits for a live terminal. Inputs: the node, whether the
 * terminal is live and the send action, which says whether the prompt was queued. Output: the
 * form. */
export function Composer({ node, live, onSend }: { node: GraphNode; live: boolean; onSend: (text: string) => boolean }) {
  const [text, setText] = useState("");
  const id = `tg-prompt-${node.id}`;
  const copy = promptCopy(node);
  const check = promptText(text);
  const send = () => {
    if (!live || check.kind !== "ok") return;
    if (onSend(check.text)) setText("");
  };
  return <form className="tg-composer" onSubmit={(event) => { event.preventDefault(); send(); }}>
    <label htmlFor={id} className="pj-sr">{displayText(copy.label)}</label>
    <textarea id={id} className="pj-input tg-composer-input" rows={2} value={text}
      placeholder={displayText(copy.placeholder)} aria-describedby={`${id}-help`}
      onChange={(event) => setText(event.target.value)}
      onKeyDown={(event) => { if (sendsMessage(event.nativeEvent)) { event.preventDefault(); send(); } }} />
    <div className="tg-composer-foot">
      <span className="tg-terminal-note" id={`${id}-help`}>{!live ? "Sends once the terminal is live." : check.kind === "too_long" ? check.reason : "Sends the text as one message, then Enter. Enter to send, Shift+Enter for a new line."}</span>
      <Button type="submit" variant="primary" size="small" icon="send" disabled={!live || check.kind !== "ok"}>Send</Button>
    </div>
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
 * the send queue, the send call for one chunk, the live check and whether keys may reach
 * the terminal; without keys only the wheel does. Output: the subscription. */
export function connectInput(terminal: Terminal, run: (job: () => Promise<boolean>) => boolean, send: (chunk: number[]) => Promise<boolean>, live: () => boolean, keys: () => boolean = () => true): IDisposable {
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

/** One agent's live terminal beside the graph: the screen at attach, live output, and every
 * key typed straight into the agent's real terminal, sized to the panel, with a way back to
 * the latest output once scrolled up, and Copy and Copy output on right-click and in its
 * status line. Closing it detaches the view; the agent keeps running.
 * Inputs: the graph node, a harness name lookup, the close action when the panel can be
 * closed, `embedded` when a console around it owns the head, and where the owner types:
 * the terminal, the message box under it, or both. Output: the panel. */
export function TerminalPanel({ node, harness, onClose, embedded = false, input = "both" }: {
  node: GraphNode;
  harness?: (id: string) => string;
  onClose?: () => void;
  embedded?: boolean;
  input?: InputChoice;
}) {
  const mode = inputMode(input);
  const keys = useRef(mode.keys);
  keys.current = mode.keys;
  const screen = useRef<HTMLDivElement>(null);
  const prompt = useRef<((text: string) => boolean) | null>(null);
  const latest = useRef<(() => void) | null>(null);
  const texts = useRef<{ selection: () => string; output: () => string } | null>(null);
  const menuList = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<View>({ kind: "connecting" });
  const [behind, setBehind] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; selection: string } | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    if (!menu) return;
    menuList.current?.querySelector<HTMLElement>("[role=menuitem]:not(:disabled)")?.focus();
    const outside = (event: PointerEvent) => { if (!(event.target instanceof Node && menuList.current?.contains(event.target))) setMenu(null); };
    document.addEventListener("pointerdown", outside, true);
    return () => document.removeEventListener("pointerdown", outside, true);
  }, [menu]);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(null), COPIED_MS);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const copy = async (text: string, done: string) => {
    setMenu(null);
    setCopied(await copyText(text) ? done : "The clipboard refused the copy.");
  };
  const [round, setRound] = useState(0);
  const name = node.terminal;

  useEffect(() => {
    const element = screen.current;
    if (!name || !element) return;
    setView({ kind: "connecting" });
    let disposed = false;
    let serial: number | null = null;
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
        if (!disposed) setView({ kind: "ended", copy });
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
      const typing = connectInput(terminal, run, async (chunk) => (await typeIntoTerminal(name, chunk)).kind === "ok", () => live, () => keys.current);
      prompt.current = (text) => {
        if (!live || !run(async () => (await promptTerminal(name, text)).kind === "ok")) return false;
        terminal.focus();
        return true;
      };
      const scrolled = terminal.onScroll(() => {
        const buffer = terminal.buffer.active;
        setBehind(buffer.viewportY < buffer.baseY);
      });
      latest.current = () => {
        terminal.scrollToBottom();
        terminal.focus();
      };
      texts.current = { selection: () => terminal.getSelection(), output: () => terminalOutput(terminal) };
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
        texts.current = null;
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
      const result = await attachTerminal(name, channel);
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
      release();
      if (serial !== null) void detachTerminal(name, serial);
    };
  }, [name, round]);

  return <section className={embedded ? "tg-terminal embedded" : "tg-terminal"} aria-label={`${node.label} terminal`}>
    {!embedded && <div className="tg-terminal-head">
      <span className="tg-terminal-title"><Icon name="square-terminal" size={15} />{displayText(nodeLine(node, harness))}</span>
      {node.account && <span className="tg-terminal-detail">{displayText(node.account)}</span>}
      {onClose && <Button variant="quiet" size="small" icon="x" iconOnly="Close terminal" onClick={onClose} />}
    </div>}
    {!name && <p className="tg-terminal-state" role="status">This agent has no terminal yet. It opens once the agent starts.</p>}
    {view.kind === "not_ready" && <NotReady what="Agent terminals" compact />}
    {view.kind === "connecting" && name && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Connecting to the terminal...</p>}
    {view.kind === "ended" && <div className="tg-terminal-state" role="alert">
      <span>{view.copy}</span>
      <Button size="small" icon="refresh-cw" onClick={() => setRound((value) => value + 1)}>Open again</Button>
    </div>}
    <div className="tg-terminal-screen" ref={screen} hidden={!name || view.kind === "not_ready"} onContextMenu={(event) => {
      if (!texts.current) return;
      event.preventDefault();
      const box = (event.currentTarget.parentElement ?? event.currentTarget).getBoundingClientRect();
      setMenu({ x: event.clientX - box.left, y: event.clientY - box.top, selection: texts.current.selection() });
    }} />
    {menu && <div ref={menuList} className="menu-list tg-terminal-menu" role="menu" aria-label="Terminal" style={{ left: menu.x, top: menu.y }}
      onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); setMenu(null); } }}>
      <button type="button" role="menuitem" className="menu-item" disabled={!menu.selection} onClick={() => void copy(menu.selection, "Copied the selection.")}><Icon name="copy" size={14} />Copy</button>
      <button type="button" role="menuitem" className="menu-item" onClick={() => void copy(texts.current?.output() ?? "", "Copied the output.")}><Icon name="file-text" size={14} />Copy output</button>
    </div>}
    {view.kind === "live" && <div className="tg-terminal-note">
      <span role="status">{copied ?? inputNote(node, input)}</span>
      {behind && <Button variant="quiet" size="small" icon="chevron-down" onClick={() => latest.current?.()}>Jump to latest</Button>}
      <Button variant="quiet" size="small" icon="copy" onClick={() => void copy(texts.current?.output() ?? "", "Copied the output.")}>Copy output</Button>
    </div>}
    {mode.box && name && view.kind !== "not_ready" && <Composer node={node} live={view.kind === "live"} onSend={(text) => prompt.current?.(text) ?? false} />}
  </section>;
}
