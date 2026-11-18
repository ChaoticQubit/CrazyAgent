import { Channel } from "@tauri-apps/api/core";
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { promptTerminal } from "../../api";
import { readTerminalInput } from "../../state";
import { Button, Icon, NotReady } from "../../ui";
import { displayText } from "../conversation";
import { keyId, type Held, type KeptKey } from "./kept-terminals";
import {
  closeNativeTerminal, copyNativeTerminal, coverNativeTerminal, focusNativeTerminal, jumpNativeTerminal, keysNativeTerminal, openNativeTerminal, pasteNativeTerminal,
  placeNativeTerminal, styleNativeTerminals,
  type NativeCopy, type NativeEvent, type NativeRect, type NativeStyle, type NativeTheme,
} from "./native-terminal";
import { connectingLine, endCopy, followTry, inputMode, jobQueue } from "./terminal-io";
import {
  COPIED, COPIED_MS, COPY_REFUSED, Composer, CopyMenu, FOLLOW_MS, TerminalHead, heldNativeView, keptNativeView, liveNote, rememberKeptSize, still,
  teammatePane, terminalOptions, trimKept, type PanelProps,
} from "./terminal-panel";

/** How often a shown view checks its place, beside the resize, scroll and page change triggers. */
const PLACE_CHECK_MS = 500;

/** Page elements that draw over a terminal, the app's toast among them: the native view draws
 * above the page, so it is covered while one of them overlaps it. */
const OVERLAYS = '[role="dialog"],[role="menu"],[role="listbox"],[role="tooltip"],dialog[open],[popover],.toast';

/** End codes after which the agent may be found again, so a following view opens again. */
const FOLLOWED = new Set(["terminal_ended", "terminal_unavailable", "terminal_closed"]);

/** The note when a prompt stopped reaching the agent. */
const STOPPED_TYPING = "Typing stopped reaching this agent. Open the terminal again to reconnect.";
const SEED_REFUSED = "The drafted message wasn't pasted.";

const THEME_KEYS: readonly (keyof NativeTheme)[] = [
  "background", "foreground", "cursor", "cursorAccent", "selectionBackground", "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
];

type View = { kind: "connecting"; reason?: string; tries?: number } | { kind: "live" } | { kind: "ended"; copy: string } | { kind: "not_ready" };

/** What a native view shows: its state, whether the owner reads above the latest output,
 * whether the app in it reads the mouse, and where its right-click menu is open, if it is. */
type NativeState = { view: View; behind: boolean; mouse: boolean; menu: { x: number; y: number } | null };

/** One open of a native view: its id once the desktop answers, whether it ended or was let
 * go, when its ready event came (it is live once both are in), whether the desktop lets its
 * keys through, and its own prompt queue, which stops for good with the open. */
type Opened = { id: number | null; ended: boolean; closed: boolean; liveAt: number | null; keys: boolean; send: (job: () => Promise<boolean>) => boolean };

const IDLE: NativeState = { view: { kind: "connecting" }, behind: false, mouse: false, menu: null };

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The native style from the app's terminal settings: its mono font and size, the blinking
 * cursor and the 21 terminal colours. Output: the style. */
export function nativeStyle(): NativeStyle {
  const options = terminalOptions();
  const colours: Record<string, unknown> = { ...options.theme };
  const theme = {} as NativeTheme;
  for (const key of THEME_KEYS) {
    const colour = colours[key];
    assert(typeof colour === "string" && colour.length > 0, `the terminal theme has ${key}`);
    theme[key] = colour;
  }
  assert(typeof options.fontFamily === "string" && typeof options.fontSize === "number", "the terminal options name a font");
  return { fontFamily: options.fontFamily, fontSize: options.fontSize, cursorBlink: options.cursorBlink === true, theme };
}

let styled = false;

/** Sends the app's terminal style to every native view again once the owner's appearance or
 * the system's changes, once per window. Output: none. */
function followStyle(): void {
  if (styled) return;
  styled = true;
  const apply = () => void styleNativeTerminals(nativeStyle());
  new MutationObserver(apply).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", apply);
}

function rectOf(element: Element): NativeRect {
  const box = element.getBoundingClientRect();
  return { x: box.left, y: box.top, width: box.width, height: box.height };
}

/** Whether a page overlay covers a place: one of `OVERLAYS` with a size, outside and not
 * around the screen, overlapping it. Inputs: the place, the screen and the overlay candidates.
 * Output: the answer. */
export function overlayCovers(rect: NativeRect, screen: Element, candidates: Iterable<Element>): boolean {
  if (rect.width < 1 || rect.height < 1) return false;
  for (const node of candidates) {
    if (screen.contains(node) || node.contains(screen)) continue;
    const box = node.getBoundingClientRect();
    if (box.width < 1 || box.height < 1) continue;
    if (box.left < rect.x + rect.width && box.right > rect.x && box.top < rect.y + rect.height && box.bottom > rect.y) return true;
  }
  return false;
}

/** One agent's native terminal view that outlives its panel (DEC-187, DEC-203): opened over a
 * panel's screen, or covered at app start; shown = placed and uncovered, hidden = covered with
 * its terminal live, let go = closed; covered under page overlays. An ended view is gone: a
 * following one opens anew after a pause, Open again at once. Inputs: the key, the agent's
 * label and the whole-pixel size its panel last had, if known. */
export class NativeView implements Held {
  readonly key: KeptKey;
  readonly id: string;
  shown = false;
  follow = false;
  private readonly label: string;
  private state: NativeState = IDLE;
  private readonly heard = new Set<() => void>();
  private screen: HTMLElement | null = null;
  private size: { width: number; height: number } | null;
  private open: Opened | null = null;
  private started = false;
  private told = true;
  private wantsFocus = false;
  private keys = inputMode(readTerminalInput()).keys;
  private placed = "";
  private tries = 0;
  private dropped = false;
  private againTimer = 0;
  private frame = 0;
  private unwatch = () => {};

  constructor(key: KeptKey, label: string, size: { width: number; height: number } | null = null) {
    assert(size === null || (size.width > 0 && size.height > 0), "a parked size is never empty");
    assert(label.length > 0, "a native view has a label");
    this.key = key;
    this.id = keyId(key);
    this.label = label;
    this.size = size;
    followStyle();
  }

  /** Whether the view was let go. Output: the answer. */
  get gone(): boolean {
    return this.dropped;
  }

  /** The size in px the view's panel last had, if known. Output: the size, or null. */
  get parked(): { width: number; height: number } | null {
    return this.size;
  }

  /** Hears every change of what the view shows, for `useSyncExternalStore`. Input: the
   * listener. Output: the call that stops it. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.heard.add(listener);
    return () => {
      this.heard.delete(listener);
    };
  };

  /** What the view shows now. Output: the same object until something in it changes. */
  readonly snapshot = (): NativeState => this.state;

  /** Opens the view covered at the size its panel last had, as app start does for a
   * remembered terminal. Output: none. */
  load(): void {
    assert(!this.shown && this.size !== null, "only a hidden view with a known size loads covered");
    if (!this.started) this.connect();
  }

  /** Shows the view over a panel's screen: opens it there on first use, or places and
   * uncovers the open one and gives it the keyboard if live; a view that ended opens again,
   * as a panel opened anew would. Input: the screen element. Output: none. */
  show(screen: HTMLElement): void {
    assert(!this.dropped, "a view let go is never shown");
    this.shown = true;
    this.screen = screen;
    if (this.state.view.kind === "ended") this.reopen();
    else if (!this.started) this.connect();
    else {
      this.watch();
      this.focus();
    }
  }

  private again(): void {
    if (this.dropped || this.open !== null) return;
    if (this.shown || this.size !== null) this.connect();
    else this.started = false;
  }

  /** Hides the view: covers it, keeping its terminal and output live. Output: none. */
  hide(): void {
    this.shown = false;
    this.wantsFocus = false;
    this.unwatch();
    this.screen = null;
    const id = this.open?.id ?? null;
    if (id !== null && !this.told) {
      this.told = true;
      void coverNativeTerminal(id, true);
    }
    this.set({ menu: null });
  }

  /** Scrolls the view to the latest output and gives it the keyboard. Output: none. */
  latest(): void {
    const id = this.open?.id ?? null;
    if (id === null) return;
    void jumpNativeTerminal(id);
    this.focus();
  }

  /** Gives the shown live view the keyboard, as the web panel focuses its terminal: at once
   * when uncovered, else once a page overlay stops covering it if the view is still live; a
   * hidden view gets nothing. Output: none. */
  focus(): void {
    const id = this.open?.id ?? null;
    if (!this.shown || id === null || this.state.view.kind !== "live") return;
    this.wantsFocus = this.told;
    if (!this.told) void focusNativeTerminal(id);
  }

  /** Lets the owner's typed keys through to the view or not, per the Terminal input choice;
   * each open carries it, and an open view is told again on every change. Input: whether keys
   * go through. Output: none. */
  setKeys(on: boolean): void {
    this.keys = on;
    this.tellKeys();
  }

  private tellKeys(): void {
    const open = this.open;
    if (open === null || open.id === null || open.ended || open.keys === this.keys) return;
    open.keys = this.keys;
    void keysNativeTerminal(open.id, this.keys);
  }

  /** Copies the view's selection or whole output. Input: what to copy. Output: the note to
   * show: the copy, or why it was refused. */
  async copy(what: NativeCopy): Promise<string> {
    const id = this.open?.id ?? null;
    if (id === null) return COPY_REFUSED;
    const result = await copyNativeTerminal(id, what);
    if (result.kind === "ok") return COPIED[what];
    return result.kind === "error" ? result.message : COPY_REFUSED;
  }

  /** Pastes a drafted message into the live view as the owner's paste would, then gives the
   * view the keyboard, as today's panel does with a console seed. Input: the text. Output:
   * null once the view took it, or the plain reason it did not. */
  async paste(text: string): Promise<string | null> {
    const id = this.open?.id ?? null;
    if (id === null || this.state.view.kind !== "live") return "The terminal is not live yet.";
    const result = await pasteNativeTerminal(id, text);
    if (result.kind === "not_ready") return "The terminal is not ready yet.";
    if (result.kind === "error") return result.message;
    this.focus();
    return null;
  }

  /** Closes the right-click menu. Output: none. */
  closeMenu(): void {
    this.set({ menu: null });
  }

  /** Sends a prompt to the agent as one message while the view is live, after any sent
   * before it, then gives the view the keyboard. Input: the text. Output: whether it was
   * queued. */
  prompt(text: string): boolean {
    const open = this.open;
    if (this.state.view.kind !== "live" || open === null) return false;
    const { terminal, pane } = this.key;
    if (!open.send(async () => (await promptTerminal(terminal, text, pane)).kind === "ok")) return false;
    this.focus();
    return true;
  }

  /** Opens a new view after the owner's Open again, with the follow count started afresh.
   * Output: none. */
  reopen(): void {
    assert(!this.dropped, "a view let go never opens again");
    this.tries = 0;
    window.clearTimeout(this.againTimer);
    this.release();
    this.set({ view: { kind: "connecting" }, behind: false, mouse: false, menu: null });
    this.again();
  }

  /** Lets the view go: closes it on the desktop, or once its open answers. The agent keeps
   * running. Output: none. */
  drop(): void {
    if (this.dropped) return;
    this.dropped = true;
    this.shown = false;
    window.clearTimeout(this.againTimer);
    this.unwatch();
    this.release();
    this.screen = null;
    this.heard.clear();
  }

  private set(next: Partial<NativeState>): void {
    const state = { ...this.state, ...next };
    if (state.view === this.state.view && state.behind === this.state.behind && state.mouse === this.state.mouse && state.menu === this.state.menu) return;
    this.state = state;
    for (const listener of [...this.heard]) listener();
  }

  private release(): void {
    this.unwatch();
    const open = this.open;
    this.open = null;
    if (open === null) return;
    open.closed = true;
    if (open.id !== null && !open.ended) void closeNativeTerminal(open.id);
  }

  private connect(): void {
    assert(this.open === null && !this.dropped, "a view opens with no open held");
    this.started = true;
    const screen = this.shown ? this.screen : null;
    const covered = screen === null;
    assert(!covered || this.size !== null, "a covered open knows its size");
    if (screen) this.keepSize(screen);
    const rect = screen ? rectOf(screen) : { x: 0, y: 0, width: this.size?.width ?? 0, height: this.size?.height ?? 0 };
    const open: Opened = { id: null, ended: false, closed: false, liveAt: null, keys: this.keys, send: () => false };
    open.send = jobQueue(() => this.stopTyping(open));
    this.open = open;
    this.told = covered;
    this.placed = `${rect.x},${rect.y},${rect.width},${rect.height}`;
    const events = new Channel<NativeEvent>((event) => this.heardEvent(open, event));
    const { terminal: name, pane } = this.key;
    void openNativeTerminal({ name, pane, label: this.label, rect, style: nativeStyle(), events, covered, keys: this.keys }).then((result) => {
      if (open.closed) {
        if (result.kind === "ok" && !open.ended) void closeNativeTerminal(result.value);
        return;
      }
      if (result.kind === "ok") {
        open.id = result.value;
        if (open.ended) return;
        this.tellKeys();
        if (this.shown) this.watch();
        else if (!this.told) {
          this.told = true;
          void coverNativeTerminal(result.value, true);
        }
        if (open.liveAt !== null) this.goLive();
      } else if (open.ended || open !== this.open) {
        return;
      } else if (result.kind === "not_ready") {
        this.open = null;
        this.set({ view: { kind: "not_ready" } });
      } else {
        this.end(open, result.code.startsWith("terminal_") ? endCopy(result.code) : result.message, !FOLLOWED.has(result.code));
      }
    });
  }

  private heardEvent(open: Opened, event: NativeEvent): void {
    if (open !== this.open || open.ended || open.closed) return;
    if (event.type === "ready") {
      open.liveAt = performance.now();
      if (open.id !== null) this.goLive();
    } else if (event.type === "menu") this.set({ menu: { x: event.x, y: event.y } });
    else if (event.type === "mouse") this.set({ mouse: event.on });
    else if (event.type === "behind") this.set({ behind: event.behind });
    else this.end(open, endCopy(event.code), event.final);
  }

  private goLive(): void {
    this.set({ view: { kind: "live" } });
    this.focus();
  }

  private end(open: Opened, copy: string, final: boolean): void {
    if (open.ended || open !== this.open) return;
    open.ended = true;
    this.unwatch();
    this.open = null;
    const next = this.follow && !final ? followTry(this.tries, open.liveAt, performance.now()) : null;
    if (next === null) {
      this.set({ view: { kind: "ended", copy }, behind: false, mouse: false, menu: null });
      return;
    }
    this.tries = next;
    this.set({ view: { kind: "connecting", reason: copy, tries: next }, behind: false, mouse: false, menu: null });
    window.clearTimeout(this.againTimer);
    this.againTimer = window.setTimeout(() => this.again(), FOLLOW_MS);
  }

  private stopTyping(open: Opened): void {
    if (open !== this.open || open.ended || this.dropped) return;
    this.release();
    this.set({ view: { kind: "ended", copy: STOPPED_TYPING }, behind: false, mouse: false, menu: null });
  }

  private keepSize(screen: HTMLElement): void {
    const width = screen.offsetWidth;
    const height = screen.offsetHeight;
    if (width < 1 || height < 1 || (this.size?.width === width && this.size.height === height)) return;
    this.size = { width, height };
    rememberKeptSize(this);
  }

  private watch(): void {
    this.unwatch();
    const screen = this.screen;
    if (!this.shown || screen === null || (this.open?.id ?? null) === null) return;
    const soon = () => {
      if (this.frame !== 0) return;
      this.frame = window.requestAnimationFrame(() => {
        this.frame = 0;
        this.check();
      });
    };
    const resized = new ResizeObserver(soon);
    resized.observe(screen);
    resized.observe(document.documentElement);
    const changed = new MutationObserver(soon);
    changed.observe(document.body, { childList: true, subtree: true });
    window.addEventListener("scroll", soon, true);
    window.addEventListener("resize", soon);
    const timer = window.setInterval(soon, PLACE_CHECK_MS);
    this.unwatch = () => {
      window.clearInterval(timer);
      window.removeEventListener("scroll", soon, true);
      window.removeEventListener("resize", soon);
      changed.disconnect();
      resized.disconnect();
      window.cancelAnimationFrame(this.frame);
      this.frame = 0;
      this.unwatch = () => {};
    };
    this.check();
  }

  private check(): void {
    const id = this.open?.id ?? null;
    const screen = this.screen;
    if (!this.shown || screen === null || id === null) return;
    const rect = rectOf(screen);
    const empty = rect.width < 1 || rect.height < 1;
    const spot = `${rect.x},${rect.y},${rect.width},${rect.height}`;
    if (!empty && spot !== this.placed) {
      this.placed = spot;
      void placeNativeTerminal(id, rect);
    }
    this.keepSize(screen);
    const covered = empty || overlayCovers(rect, screen, document.querySelectorAll(OVERLAYS));
    if (covered !== this.told) {
      this.told = covered;
      void coverNativeTerminal(id, covered);
    }
    if (!covered && this.wantsFocus) {
      this.wantsFocus = false;
      if (this.state.view.kind === "live") void focusNativeTerminal(id);
    }
  }
}

const idle = () => IDLE;

/** One agent's terminal on a build that draws terminals natively (DEC-203): the webview
 * panel's head, states, status line, menu and message box over a native view on its screen.
 * The main window keeps the view covered after the panel closes, among the two most recently
 * used (DEC-187); a wall tile's view closes with the tile. Input: `PanelProps`. Output: the
 * panel. */
export function NativeTerminalPanel({ node, harness = (id) => id, onClose, onStop, embedded = false, tile = false, swaps = false, follow = false, reported, projectId, onOpened, seed = null }: PanelProps) {
  assert(!(embedded && tile), "a terminal is either embedded in a console or a wall tile, never both");
  assert(!(embedded && swaps), "only a terminal with its own head says it swaps");
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
  const [made, setMade] = useState<NativeView | null>(null);
  const found = key && !tile ? heldNativeView(key) : null;
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
  const [copied, setCopied] = useState<string | null>(null);
  const [refused, setRefused] = useState<string | null>(null);
  const pasted = useRef<number | null>(null);
  useEffect(() => {
    const shown = current.current;
    if (seed === null || !open || shown === null || pasted.current === seed.id) return;
    pasted.current = seed.id;
    void shown.paste(seed.text).then((reason) => {
      seed.used();
      if (reason !== null) setRefused(`${SEED_REFUSED} ${reason}`);
    });
  }, [seed, open]);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(null), COPIED_MS);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const closeMenu = useRef((note: string | null) => {
    current.current?.closeMenu();
    if (note) setCopied(note);
  }).current;
  const backToView = useRef((note: string | null) => {
    closeMenu(note);
    current.current?.focus();
  }).current;

  useLayoutEffect(() => {
    if (!entry) return;
    entry.follow = following.current;
    entry.setKeys(keys.current);
  });

  useLayoutEffect(() => {
    const element = screen.current;
    if (!key || !element) return;
    const shown = tile ? new NativeView(key, node.label) : keptNativeView(key, node.label);
    shown.follow = following.current;
    shown.setKeys(keys.current);
    shown.show(element);
    if (shown !== current.current) setMade(shown);
    return () => {
      shown.hide();
      if (tile) shown.drop();
      else trimKept();
    };
  }, [id, tile]);

  const boxed = mode.box && Boolean(name) && view.kind !== "not_ready";
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
    {refused && <div className="tg-terminal-state" role="alert">
      <span>{refused}</span>
      <Button size="small" onClick={() => setRefused(null)}>Dismiss</Button>
    </div>}
    <div className="tg-terminal-screen tg-terminal-native" ref={screen} hidden={!name || view.kind === "not_ready"} data-native-terminal={name ?? undefined} />
    {state.menu && entry && <CopyMenu at={state.menu} selection copy={(what) => entry.copy(what)} back={backToView} onDone={closeMenu} />}
    {view.kind === "live" && <div className="tg-terminal-note">
      <span role="status">{copied ?? liveNote(node, input, state.mouse)}</span>
      {state.behind && <Button variant="quiet" size="small" icon="chevron-down" onClick={() => entry?.latest()}>Jump to latest</Button>}
      <Button variant="quiet" size="small" icon="copy" onClick={() => void entry?.copy("output").then(setCopied)}>Copy output</Button>
    </div>}
    {boxed && <Composer node={node} live={view.kind === "live"} onSend={(text) => entry?.prompt(text) ?? false} compact={!embedded} box={box} />}
  </section>;
}
