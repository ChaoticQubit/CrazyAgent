import { Channel } from "@tauri-apps/api/core";
import type { ITerminalOptions } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";
import { attachTerminal, detachTerminal, promptTerminal, resizeTerminal, typeIntoTerminal } from "../../api";
import type { GraphNode, TerminalMessage } from "../../engine-client";
import { Button, Icon, NotReady, StatusBadge, sendsMessage } from "../../ui";
import { displayText } from "../conversation";
import { nodeDetail, nodeStatus } from "./graph";
import { encodeBinary, encodeTyped, endCopy, jobQueue, keyQueue, muteQueries, promptText, settleAfterWrites, terminalEvent, terminalSize } from "./terminal-io";

const RESIZE_PAUSE_MS = 120;
const SCROLLBACK_LINES = 5000;
const STOPPED_TYPING = "Typing stopped reaching this agent. Open the terminal again to reconnect.";

type View = { kind: "connecting" } | { kind: "live" } | { kind: "ended"; copy: string } | { kind: "not_ready" };

function token(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** The view's xterm settings: the app's mono font and terminal colors, Option as Meta and
 * `SCROLLBACK_LINES` of scrollback for the history the engine sends on attach. Output: the
 * options for a new terminal. */
export function terminalOptions(): ITerminalOptions {
  return {
    cursorBlink: true,
    fontFamily: token("--mono", "ui-monospace, Menlo, monospace"),
    fontSize: 13,
    macOptionIsMeta: true,
    scrollback: SCROLLBACK_LINES,
    theme: {
      background: token("--terminal-bg", "#161513"),
      foreground: token("--terminal-fg", "#eeeae2"),
      cursor: token("--terminal-cursor", "#ff6a45"),
      cursorAccent: token("--terminal-bg", "#161513"),
      selectionBackground: token("--terminal-selection", "rgba(255, 106, 69, 0.32)"),
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

/** One agent's live terminal beside the graph: the screen at attach, live output, and every
 * key typed straight into the agent's real terminal, sized to the panel. Closing it detaches
 * the view; the agent keeps running. Inputs: the graph node, a harness name lookup and the
 * close action. Output: the panel. */
export function TerminalPanel({ node, harness, onClose }: {
  node: GraphNode;
  harness?: (id: string) => string;
  onClose: () => void;
}) {
  const screen = useRef<HTMLDivElement>(null);
  const prompt = useRef<((text: string) => boolean) | null>(null);
  const [view, setView] = useState<View>({ kind: "connecting" });
  const [round, setRound] = useState(0);
  const name = node.terminal;
  const status = nodeStatus(node);

  useEffect(() => {
    const element = screen.current;
    if (!name || !element) return;
    setView({ kind: "connecting" });
    let disposed = false;
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
      const type = keyQueue(run, async (chunk) => (await typeIntoTerminal(name, chunk)).kind === "ok");
      const typed = terminal.onData((data) => { if (live) type(encodeTyped(data)); });
      prompt.current = (text) => {
        if (!live || !run(async () => (await promptTerminal(name, text)).kind === "ok")) return false;
        terminal.focus();
        return true;
      };
      const binary = terminal.onBinary((data) => { if (live) type(encodeBinary(data)); });
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
        channel.onmessage = () => {};
        observer.disconnect();
        typed.dispose();
        binary.dispose();
        resized.dispose();
        window.clearTimeout(timer);
        window.cancelAnimationFrame(frame);
        unsettle();
        terminal.dispose();
      };
      const result = await attachTerminal(name, channel);
      if (disposed) {
        if (result.kind === "ok") void detachTerminal(name);
        return;
      }
      if (result.kind === "not_ready") setView({ kind: "not_ready" });
      else if (result.kind === "error") end(result.code.startsWith("terminal_") ? endCopy(result.code) : result.message);
    })();
    return () => {
      disposed = true;
      release();
      void detachTerminal(name);
    };
  }, [name, round]);

  return <section className="tg-terminal" aria-label={`${node.label} terminal`}>
    <div className="tg-terminal-head">
      <span className="tg-terminal-title"><Icon name="square-terminal" size={15} />{displayText(node.label)}</span>
      <span className="tg-terminal-detail">{displayText(nodeDetail(node, harness))}</span>
      {status && <StatusBadge word={status.word} tone={status.tone} />}
      <Button variant="quiet" size="small" icon="x" iconOnly="Close terminal" onClick={onClose} />
    </div>
    {!name && <p className="tg-terminal-state" role="status">This agent has no terminal yet. It opens once the agent starts.</p>}
    {view.kind === "not_ready" && <NotReady what="Agent terminals" compact />}
    {view.kind === "connecting" && name && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Connecting to the terminal...</p>}
    {view.kind === "ended" && <div className="tg-terminal-state" role="alert">
      <span>{view.copy}</span>
      <Button size="small" icon="refresh-cw" onClick={() => setRound((value) => value + 1)}>Open again</Button>
    </div>}
    <div className="tg-terminal-screen" ref={screen} hidden={!name || view.kind === "not_ready"} />
    {view.kind === "live" && <p className="tg-terminal-note">{typingNote(node)}</p>}
    {name && view.kind !== "not_ready" && <Composer node={node} live={view.kind === "live"} onSend={(text) => prompt.current?.(text) ?? false} />}
  </section>;
}
