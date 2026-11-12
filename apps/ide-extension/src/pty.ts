import type { Socket } from "node:net";
import { dirname, isAbsolute } from "node:path";
import { FrameReader, encodeFrame, type Disposable } from "./engine.js";

/** Smallest terminal side, in cells, the engine accepts. */
export const SIZE_MIN = 2;

/** Largest terminal side, in cells, the engine accepts. */
export const SIZE_MAX = 1000;

/** Largest input, in bytes, one engine terminal frame carries. */
export const INPUT_BYTES_MAX = 4096;

/** What the terminal shows where the engine has no qualified sandbox for it. */
export const UNAVAILABLE =
  "This terminal is unavailable: this system has no qualified sandbox for it.";

/** What the terminal shows when the engine already runs its most confined terminals. */
export const BUSY = "This terminal can't start: too many are open. Close one, then open a new terminal.";

/** What the terminal shows when its level is above the project's access. */
export const ABOVE_PROJECT = "Raise project access to use this";

const SIZE_DEFAULT: Size = { columns: 80, rows: 24 };
const CODE = /^[a-z0-9_]{1,64}$/;

/** The level a terminal asks the engine for; the engine decides whether the project allows it. */
export type Level = "sandboxed" | "own_branch";

/** The variable the engine names the project's access level in. */
export const ACCESS_ENV = "CRAZYAGENT_IDE_ACCESS";

/** A project's access level, as the engine names it. */
export type Access = Level | "full";

/**
 * The project's access level for the terminal menu. It only shapes the menu, since the engine
 * clamps every terminal itself. Input: the variable's value. Output: the level, the narrowest
 * for a missing or unknown value.
 */
export function accessOf(value: string | undefined): Access {
  return value === "own_branch" || value === "full" ? value : "sandboxed";
}

/** A terminal size in cells, as VS Code reports it. */
export interface Size {
  columns: number;
  rows: number;
}

/** Opens one streaming bridge call and resolves to its socket, as `Engine.stream` does. */
export type Open = (name: string, args: Record<string, unknown>) => Promise<Socket>;

/** What the engine sends on an `ide.terminal` connection. */
type Pushed =
  | { kind: "ok"; data: { terminal: string; cols: number; rows: number } }
  | { kind: "rejected"; code: string }
  | { kind: "output"; bytes: number[] }
  | { kind: "closed"; reason: string };

/** The part of VS Code's environment variable collection that puts a folder first on PATH. */
export interface PathPrepender {
  prepend(variable: string, value: string): void;
}

/**
 * Puts the folder of the engine's `git.path` shim first on PATH in the terminals the IDE
 * server hosts, so `git` there runs the real git the engine resolved and not macOS's
 * `/usr/bin/git`, whose cache write the IDE's sandbox refuses on every call. VS Code applies a
 * PATH prepend again after a login shell's profile, which would otherwise put `/usr/bin` first.
 * Inputs: the collection and the `git.path` setting. Output: none; a value that is not an
 * absolute path, or a folder PATH cannot hold, changes nothing.
 */
export function gitFirstOnPath(collection: PathPrepender, gitPath: unknown): void {
  if (typeof gitPath !== "string" || !isAbsolute(gitPath)) return;
  const folder = dirname(gitPath);
  if (folder.includes(":")) return;
  collection.prepend("PATH", `${folder}:`);
}

/** An event source with VS Code's `Event` shape: subscribe a listener, get a disposable. */
class Signal<T> {
  readonly #listeners = new Set<(value: T) => void>();

  readonly event = (listener: (value: T) => void): Disposable => {
    this.#listeners.add(listener);
    return { dispose: () => this.#listeners.delete(listener) };
  };

  fire(value: T): void {
    for (const listener of [...this.#listeners]) listener(value);
  }
}

/**
 * Keeps one terminal side within the engine's bounds. Input: a size from VS Code.
 * Output: the size, clamped to `SIZE_MIN..SIZE_MAX`, or the default for a non-number.
 */
export function clampSize(size: Size | undefined): Size {
  const side = (value: number | undefined, fallback: number) =>
    Number.isFinite(value) ? Math.min(SIZE_MAX, Math.max(SIZE_MIN, Math.trunc(value as number))) : fallback;
  return { columns: side(size?.columns, SIZE_DEFAULT.columns), rows: side(size?.rows, SIZE_DEFAULT.rows) };
}

/**
 * Explains why a confined terminal stopped, for the terminal itself. Input: the engine's
 * plain code. Output: one line; a code that is not plain text is not repeated.
 */
export function stopped(code: string): string {
  if (code === "ide_terminal_unavailable") return UNAVAILABLE;
  if (code === "ide_terminals_busy") return BUSY;
  if (code === "ide_terminal_above_project") return ABOVE_PROJECT;
  return `This terminal stopped (${CODE.test(code) ? code : "unknown"}).`;
}

/**
 * A VS Code pseudoterminal for the owner's shell that the engine runs under the project's
 * agent sandbox. `open` starts it over one `ide.terminal` connection; output, typing and
 * resizes travel as engine terminal frames, typing in chunks of at most `INPUT_BYTES_MAX`
 * bytes, and typing before the connection opens is dropped. `ready` resolves true once the
 * engine attached the terminal, so typing reaches its shell, and false when the terminal
 * stopped or closed first. The terminal closes when the shell ends; when the engine refuses or
 * stops it, the reason stays on screen. `close` detaches, which ends the shell.
 */
export class SandboxedPty {
  readonly #wrote = new Signal<string>();
  readonly #closed = new Signal<void>();
  readonly onDidWrite = this.#wrote.event;
  readonly onDidClose = this.#closed.event;
  readonly ready: Promise<boolean>;
  readonly #open: Open;
  readonly #level: Level;
  readonly #text = new TextDecoder("utf-8");
  #settle: (attached: boolean) => void = () => undefined;
  #socket: Socket | undefined;
  #size: Size = SIZE_DEFAULT;
  #attached = false;
  #ended = false;

  constructor(open: Open, level: Level) {
    this.#open = open;
    this.#level = level;
    this.ready = new Promise((resolve) => {
      this.#settle = resolve;
    });
  }

  open(initial: Size | undefined): void {
    const opened = clampSize(initial);
    this.#size = opened;
    this.#open("ide.terminal", { cols: opened.columns, rows: opened.rows, level: this.#level }).then(
      (socket) => this.#attach(socket, opened),
      (error: unknown) => this.#stop(error instanceof Error ? error.message : "unknown"),
    );
  }

  /**
   * Takes the opened connection, unless the terminal closed meanwhile, and sends a resize
   * that came before it.
   */
  #attach(socket: Socket, opened: Size): void {
    if (this.#ended) {
      socket.destroy();
      return;
    }
    this.#socket = socket;
    const reader = new FrameReader();
    socket.on("data", (chunk: Buffer) => {
      let frames: unknown[];
      try {
        frames = reader.push(chunk);
      } catch {
        this.#stop("ide_frame_invalid");
        return;
      }
      for (const frame of frames) this.#take((frame ?? {}) as Pushed);
    });
    socket.on("error", () => undefined);
    socket.on("close", () => this.#stop("ide_engine_closed"));
    if (this.#size.columns !== opened.columns || this.#size.rows !== opened.rows) this.setDimensions(this.#size);
  }

  handleInput(data: string): void {
    if (this.#ended || this.#socket === undefined) return;
    const bytes = Buffer.from(data, "utf8");
    for (let at = 0; at < bytes.length; at += INPUT_BYTES_MAX) {
      this.#send({ kind: "input", bytes: [...bytes.subarray(at, at + INPUT_BYTES_MAX)] });
    }
  }

  setDimensions(size: Size): void {
    this.#size = clampSize(size);
    if (this.#ended || this.#socket === undefined) return;
    this.#send({ kind: "resize", cols: this.#size.columns, rows: this.#size.rows });
  }

  close(): void {
    if (this.#ended) return;
    this.#ended = true;
    this.#settle(false);
    this.#send({ kind: "detach" });
    this.#socket?.end();
  }

  /** Applies one engine frame: the `ok` or refusal first, then output and the close. */
  #take(frame: Pushed): void {
    if (this.#ended) return;
    if (!this.#attached && frame.kind === "ok") {
      this.#attached = true;
      this.#settle(true);
    } else if (!this.#attached && frame.kind === "rejected") {
      this.#stop(String(frame.code));
    } else if (this.#attached && frame.kind === "output" && Array.isArray(frame.bytes)) {
      const text = this.#text.decode(Buffer.from(frame.bytes), { stream: true });
      if (text.length > 0) this.#wrote.fire(text);
    } else if (this.#attached && frame.kind === "closed") {
      this.#stop(String(frame.reason));
    } else {
      this.#stop("ide_frame_invalid");
    }
  }

  /** Ends the terminal once: closes it when the shell ended, else shows why it stopped. */
  #stop(reason: string): void {
    if (this.#ended) return;
    this.#ended = true;
    this.#settle(false);
    this.#socket?.destroy();
    const rest = this.#text.decode();
    if (rest.length > 0) this.#wrote.fire(rest);
    if (reason === "terminal_ended") {
      this.#closed.fire();
    } else {
      this.#wrote.fire(`\r\n${stopped(reason)}\r\n`);
    }
  }

  /** Writes one frame to the engine connection. */
  #send(value: unknown): void {
    this.#socket?.write(encodeFrame(value));
  }
}
