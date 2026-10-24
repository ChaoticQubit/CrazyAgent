import { Channel } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { attachTerminal, detachTerminal, resizeTerminal, typeIntoTerminal } from "../../api";
import type { GraphNode, TerminalMessage } from "../../engine-client";
import { Button, Icon, NotReady, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { nodeDetail, nodeStatus } from "./graph";
import { encodeBinary, encodeTyped, endCopy, inputQueue, terminalEvent, terminalSize } from "./terminal-io";

const RESIZE_PAUSE_MS = 120;
const SCROLLBACK_LINES = 5000;
const STOPPED_TYPING = "Typing stopped reaching this agent. Open the terminal again to reconnect.";

type View = { kind: "connecting" } | { kind: "live" } | { kind: "ended"; copy: string } | { kind: "not_ready" };

function token(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** The note under a live terminal: who reads what the owner types. Input: the node.
 * Output: one sentence. */
export function typingNote(node: GraphNode): string {
  return node.kind === "master"
    ? "Typing goes straight to the master."
    : "Typing goes straight to this agent. The master gets a note of what you type.";
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
      const terminal = new Terminal({
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
      });
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
      const type = inputQueue(async (chunk) => (await typeIntoTerminal(name, chunk)).kind === "ok", () => end(STOPPED_TYPING));
      const typed = terminal.onData((data) => { if (live) type(encodeTyped(data)); });
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
          terminal.write(event.bytes, () => {
            if (fitted || disposed) return;
            fitted = true;
            fit.fit();
            sendSize();
          });
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
        channel.onmessage = () => {};
        observer.disconnect();
        typed.dispose();
        binary.dispose();
        resized.dispose();
        window.clearTimeout(timer);
        window.cancelAnimationFrame(frame);
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
  </section>;
}
