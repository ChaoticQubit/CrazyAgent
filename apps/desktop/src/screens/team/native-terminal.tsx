import { Channel, invoke } from "@tauri-apps/api/core";
import { useLayoutEffect, useRef } from "react";

/** A rectangle in CSS pixels of the page, as `getBoundingClientRect` gives it. */
export type Rect = { x: number; y: number; w: number; h: number };

/** One timed input a native view finished: what it was, when the page dispatched it (wall
 * ms, `performance.timeOrigin` based) and when its command went to tmux, tmux took it all,
 * the pane answered, the screen showed the awaited text and the frame reached the display. */
export type NativeProbe = {
  seq: number;
  kind: string;
  t0: number;
  bytes: number;
  sent: number | null;
  written: number | null;
  accepted: number | null;
  output: number | null;
  render: number | null;
  frame: number | null;
  local: boolean;
  failed: boolean;
};

/** What a native view reports: ready, one finished timed input, or ended with a plain code. */
export type NativeReport = { type: "ready" } | { type: "ended"; code: string } | ({ type: "probe" } & NativeProbe);

/** The most finished timed inputs a page keeps for the harness to read. */
const REPORTS_MAX = 20000;

/** How often a view checks its place, beside the resize, scroll and DOM change triggers. */
const PLACE_CHECK_MS = 500;

/** Page elements that draw over a terminal: the native view hides while one covers it. */
const OVERLAYS = '[role="dialog"],[role="menu"],[role="listbox"],[role="tooltip"],dialog[open],[popover]';

/** Keys the test hook forwards as named keys; any other key arrives as text. */
const NAMED_KEYS = new Set(["Enter", "Backspace", "Tab", "Escape", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
  "Home", "End", "PageUp", "PageDown", "Delete"]);

declare global {
  interface Window {
    __nativeTerminalReports?: NativeProbe[];
    __nativeTerminalExpect?: string | null;
  }
}

let enabled = false;
let sequence = 0;

/** Reads once, before the first render, whether the desktop draws terminals natively
 * (DEC-203 research switch). Output: done; a page outside the desktop reads it as off. */
export async function loadNativeTerminal(): Promise<void> {
  try {
    enabled = (await invoke<boolean>("desktop_native_terminal_enabled")) === true;
  } catch {
    enabled = false;
  }
}

/** Whether terminals draw natively in this page. Output: the answer. */
export function nativeTerminalOn(): boolean {
  return enabled;
}

/** Keeps one finished timed input for the harness, the oldest dropped past `REPORTS_MAX`, and
 * tells listeners of `native-terminal-report`. Inputs: the kept list and the input. Output:
 * the list. */
export function keepReport(kept: NativeProbe[], report: NativeProbe): NativeProbe[] {
  if (kept.length >= REPORTS_MAX) kept.splice(0, kept.length - REPORTS_MAX + 1);
  kept.push(report);
  if (kept.length > REPORTS_MAX) throw new Error("native terminal reports are bounded");
  return kept;
}

/** Whether a page overlay covers a rectangle: one of `OVERLAYS` with a size, outside and not
 * around the terminal, overlapping it. Inputs: the rectangle, the terminal's element and the
 * overlay candidates. Output: the answer. */
export function overlayCovers(rect: Rect, element: Element, candidates: Iterable<Element>): boolean {
  if (rect.w < 1 || rect.h < 1) return false;
  for (const node of candidates) {
    if (element.contains(node) || node.contains(element)) continue;
    const box = node.getBoundingClientRect();
    if (box.width < 1 || box.height < 1) continue;
    if (box.left < rect.x + rect.w && box.right > rect.x && box.top < rect.y + rect.h && box.bottom > rect.y) return true;
  }
  return false;
}

/** Forwards the page events the latency harness dispatches on the terminal to the native
 * view's own input handlers: text, named and Control keys, Cmd-C, wheel, left-button press,
 * drag and release (points relative to the element) and paste with its text. Inputs: the
 * element and the sender, given the event fields and the event's time stamp. Output: the
 * undo. */
function hook(element: HTMLElement, send: (event: Record<string, unknown>, stamp: number) => void): () => void {
  const point = (event: MouseEvent) => {
    const box = element.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  };
  const listeners: [string, (event: Event) => void][] = [
    ["input", (event) => {
      const data = (event as InputEvent).data;
      if (data) send({ kind: "text", text: data }, event.timeStamp);
    }],
    ["keydown", (event) => {
      const key = event as KeyboardEvent;
      if (key.metaKey && key.key.toLowerCase() === "c") {
        key.preventDefault();
        send({ kind: "copy" }, key.timeStamp);
      } else if (!key.metaKey && (NAMED_KEYS.has(key.key) || (key.ctrlKey && key.key.length === 1))) {
        key.preventDefault();
        send({ kind: "key", key: key.key, ctrl: key.ctrlKey }, key.timeStamp);
      }
    }],
    ["wheel", (event) => {
      const wheel = event as WheelEvent;
      wheel.preventDefault();
      send({ kind: "wheel", dy: wheel.deltaY, mode: wheel.deltaMode, ...point(wheel) }, wheel.timeStamp);
    }],
    ["mousedown", (event) => {
      const mouse = event as MouseEvent;
      if (mouse.button === 0) send({ kind: "press", option: mouse.altKey, ...point(mouse) }, mouse.timeStamp);
    }],
    ["mousemove", (event) => {
      const mouse = event as MouseEvent;
      if ((mouse.buttons & 1) === 1) send({ kind: "drag", option: mouse.altKey, ...point(mouse) }, mouse.timeStamp);
    }],
    ["mouseup", (event) => {
      const mouse = event as MouseEvent;
      if (mouse.button === 0) send({ kind: "release", option: mouse.altKey, ...point(mouse) }, mouse.timeStamp);
    }],
    ["paste", (event) => {
      const paste = event as ClipboardEvent;
      const text = paste.clipboardData?.getData("text/plain") ?? "";
      paste.preventDefault();
      if (text) send({ kind: "paste", text }, paste.timeStamp);
    }],
  ];
  for (const [type, listener] of listeners) element.addEventListener(type, listener, { passive: false });
  return () => {
    for (const [type, listener] of listeners) element.removeEventListener(type, listener);
  };
}

/** One agent terminal drawn by a native view over this element (DEC-203): no xterm and no
 * engine attach; the view reads the engine's tmux itself. The element reports its rectangle
 * on every resize, scroll and DOM change, hides the view while a page overlay covers it,
 * and forwards the harness's page events to the view. Inputs: the agent's window name, what
 * hears the view ready and what hears it end with a plain code (the panel then falls back to
 * today's renderer). Output: the element. */
export function NativeScreen({ name, onReady, onEnded }: { name: string; onReady: () => void; onEnded: (code: string) => void }) {
  const host = useRef<HTMLDivElement>(null);
  const ready = useRef(onReady);
  ready.current = onReady;
  const ended = useRef(onEnded);
  ended.current = onEnded;
  useLayoutEffect(() => {
    const element = host.current;
    if (element === null) return;
    let id: number | null = null;
    let gone = false;
    let placed = "";
    let covered = false;
    let pending = false;
    const rect = (): Rect => {
      const box = element.getBoundingClientRect();
      return { x: box.left, y: box.top, w: box.width, h: box.height };
    };
    const place = () => {
      pending = false;
      if (id === null || gone) return;
      const now = rect();
      const key = `${now.x},${now.y},${now.w},${now.h}`;
      if (key !== placed) {
        placed = key;
        void invoke("desktop_native_terminal_place", { id, rect: now }).catch(() => undefined);
      }
      const cover = overlayCovers(now, element, document.querySelectorAll(OVERLAYS));
      if (cover !== covered) {
        covered = cover;
        void invoke("desktop_native_terminal_cover", { id, covered: cover }).catch(() => undefined);
      }
    };
    const soon = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(place);
    };
    const channel = new Channel<NativeReport>((report) => {
      if (report.type === "probe") {
        const { type: _type, ...probe } = report;
        window.__nativeTerminalReports = keepReport(window.__nativeTerminalReports ?? [], probe);
        window.dispatchEvent(new CustomEvent("native-terminal-report", { detail: probe }));
      } else if (report.type === "ready") ready.current();
      else if (!gone) ended.current(report.code);
    });
    const send = (event: Record<string, unknown>, stamp: number) => {
      if (id === null) return;
      sequence += 1;
      const payload = { ...event, seq: sequence, t0: performance.timeOrigin + stamp, expect: window.__nativeTerminalExpect ?? null };
      void invoke("desktop_native_terminal_input", { id, event: payload }).catch(() => undefined);
    };
    invoke<number>("desktop_native_terminal_open", { name, rect: rect(), output: channel })
      .then((opened) => {
        if (gone) {
          void invoke("desktop_native_terminal_close", { id: opened }).catch(() => undefined);
          return;
        }
        id = opened;
        place();
      })
      .catch((error: unknown) => {
        if (!gone) ended.current(String(error));
      });
    const resized = new ResizeObserver(soon);
    resized.observe(element);
    resized.observe(document.documentElement);
    const changed = new MutationObserver(soon);
    changed.observe(document.body, { childList: true, subtree: true });
    window.addEventListener("scroll", soon, true);
    window.addEventListener("resize", soon);
    const timer = window.setInterval(soon, PLACE_CHECK_MS);
    const unhook = hook(element, send);
    return () => {
      gone = true;
      unhook();
      window.clearInterval(timer);
      window.removeEventListener("scroll", soon, true);
      window.removeEventListener("resize", soon);
      changed.disconnect();
      resized.disconnect();
      if (id !== null) void invoke("desktop_native_terminal_close", { id }).catch(() => undefined);
    };
  }, [name]);
  return <div className="tg-terminal-screen tg-terminal-native" ref={host} tabIndex={0} data-native-terminal={name} />;
}
