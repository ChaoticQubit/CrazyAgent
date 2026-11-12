import type { HarnessEntry } from "../../api";
import type { DependencyState } from "../../engine-client";
import { displayText } from "../conversation";
import { updateCopy, type HarnessVersions } from "../../api-harness-updates";

/** The Harnesses step's place in first run, in the same words as the TUI step: its label in the
 * step list, its heading and its intro. */
export const HARNESS_STEP = {
  label: "Harnesses",
  title: "Choose your harnesses",
  intro: "A harness is the coding agent a master or agent runs in. RCLI comes with CrazyAgent; pick any others to install now, or add them later in Settings, Harnesses.",
};

/** The harnesses the step offers, in order: RCLI, which comes with CrazyAgent, then the ones
 * CrazyAgent can launch. The list grows as launch adapters land. */
export const OFFERED: readonly { id: string; name: string }[] = [
  { id: "rcli", name: "RCLI" },
  { id: "codex", name: "Codex" },
  { id: "claude-code", name: "Claude Code" },
];

/** The words each row and button of the step uses, the same as the TUI step's. */
export const HARNESS_WORDS = {
  bundled: "Comes with CrazyAgent.",
  installed: "Already on this Mac.",
  installsWith: "Installs with",
  reading: "Reading what's on this Mac.",
  waiting: "Waiting to install.",
  installing: "Installing now.",
  done: "Installed.",
  tryAgain: "Try again",
  guide: "How to install it yourself",
  install: "Install picked harnesses",
  skip: "Skip for now",
  next: "Continue",
  installNode: "Install Node.js",
  installingNode: "Installing Node.js.",
};

/** Where one offered harness stands on the step. */
export type HarnessRow =
  | { kind: "reading" }
  | { kind: "bundled" }
  | { kind: "installed"; version: string | null }
  | { kind: "offer"; line: string }
  | { kind: "refused"; reason: string; guide: string | null }
  | { kind: "waiting" }
  | { kind: "installing" }
  | { kind: "done"; version: string | null }
  | { kind: "failed"; reason: string }
  | { kind: "blocked"; line: string };

/** Where an offered harness stands, from the engine's catalog row, versions read and install
 * queue: RCLI always comes with the app; a running or queued install wins; a failed install of a
 * harness still missing shows its result; a found harness the resolver can't run says what it
 * needs; else a fresh install, installed, the install line, or why it can't install. Inputs: the
 * id, its catalog row, its versions read (null while unread) and the queued ids. Output: the row. */
export function harnessRow(id: string, entry: HarnessEntry | undefined, versions: HarnessVersions | null, queued: readonly string[]): HarnessRow {
  if (id === "rcli") return { kind: "bundled" };
  if (versions === null) return { kind: "reading" };
  if (versions.running) return { kind: "installing" };
  if (queued.includes(id)) return { kind: "waiting" };
  const present = Boolean(entry?.executable);
  const ended = versions.last;
  if (ended && !ended.ok && !present) return { kind: "failed", reason: updateCopy(ended.code ?? "harness_install_failed") };
  if (present && versions.needs) return { kind: "blocked", line: displayText(versions.needs.line) };
  if (ended && ended.ok && ended.from === null && present) return { kind: "done", version: ended.to };
  if (present) return { kind: "installed", version: versions.installed };
  if (versions.install !== null) return { kind: "offer", line: versions.install };
  return { kind: "refused", reason: updateCopy(versions.refusal ?? "harness_install_no_tool"), guide: versions.guide };
}

const BADGES: Record<HarnessRow["kind"], { word: string; tone: "ok" | "warn" | "danger" | "info" | "neutral" }> = {
  bundled: { word: "Included", tone: "ok" },
  installed: { word: "Installed", tone: "ok" },
  offer: { word: "Not installed", tone: "neutral" },
  refused: { word: "Can't install", tone: "warn" },
  reading: { word: "Reading", tone: "info" },
  waiting: { word: "Waiting", tone: "info" },
  installing: { word: "Installing", tone: "info" },
  done: { word: "Installed", tone: "ok" },
  failed: { word: "Failed", tone: "danger" },
  blocked: { word: "Can't run", tone: "warn" },
};

/** The badge word and tone a row shows, the TUI's words. Input: the row. Output: the badge. */
export function rowBadge(row: HarnessRow): { word: string; tone: "ok" | "warn" | "danger" | "info" | "neutral" } {
  return BADGES[row.kind];
}

/** The sentence under a harness's name: where it comes from, what installing it runs, its
 * result, or the plain reason it can't install. Input: the row. Output: one line. */
export function rowLine(row: HarnessRow): string {
  const version = (value: string | null) => value === null ? "" : ` Version ${value}.`;
  switch (row.kind) {
    case "bundled": return HARNESS_WORDS.bundled;
    case "installed": return `${HARNESS_WORDS.installed}${version(row.version)}`;
    case "offer": return `${HARNESS_WORDS.installsWith} ${row.line}`;
    case "refused": return row.reason;
    case "reading": return HARNESS_WORDS.reading;
    case "waiting": return HARNESS_WORDS.waiting;
    case "installing": return HARNESS_WORDS.installing;
    case "done": return `${HARNESS_WORDS.done}${version(row.version)}`;
    case "failed": return row.reason;
    case "blocked": return row.line;
  }
}

/** Tells whether the owner can pick a row to install: only one with an install line.
 * Input: the row. Output: the answer. */
export function tickable(row: HarnessRow): boolean {
  return row.kind === "offer";
}

/** What Try again does on a row: install a failed harness again, read a refused one again in
 * case its reason went away, or nothing. Input: the row. Output: the action or null. */
export function retry(row: HarnessRow): "install" | "read" | null {
  if (row.kind === "failed") return "install";
  if (row.kind === "refused" || row.kind === "blocked") return "read";
  return null;
}

/** The picked ids the engine installs: every pick but RCLI, which comes with the app.
 * Input: the picks. Output: the ids to send. */
export function installable(picks: readonly string[]): string[] {
  return picks.filter((id) => id !== "rcli");
}

const ENDED = new Set<HarnessRow["kind"]>(["done", "failed", "installed", "blocked"]);

/** How far the sent picks are: how many ended, how many failed or can't run, and whether any
 * still waits or installs. Inputs: the sent ids and the rows by id. Output: the counts. */
export function progress(sent: readonly string[], rows: Readonly<Record<string, HarnessRow>>): { done: number; total: number; failed: number; running: boolean } {
  const kinds = sent.map((id) => rows[id]?.kind ?? "reading");
  return {
    done: kinds.filter((kind) => ENDED.has(kind)).length,
    total: sent.length,
    failed: kinds.filter((kind) => kind === "failed" || kind === "blocked").length,
    running: kinds.some((kind) => !ENDED.has(kind)),
  };
}

/** The progress sentence under the rows, the TUI's words. Input: the counts. Output: the line. */
export function progressLine(counts: { done: number; total: number; failed: number; running: boolean }): string {
  if (counts.running) return `Installing: ${counts.done} of ${counts.total} done. You can continue; the installs keep going.`;
  const failed = counts.failed ? `, ${counts.failed} failed. Each failure shows its reason above` : "";
  return `Installs finished: ${counts.done} of ${counts.total} done${failed}.`;
}

const NEEDS_NODE = "harness_install_needs_node";

/** Where the step's one Node.js install stands while an offered harness needs Node, the TUI's
 * kinds: nothing to offer, the offer, the install running, or its failure with setup's reason. */
export type NodeOffer = { kind: "none" } | { kind: "offer" } | { kind: "installing" } | { kind: "failed"; reason: string };

/** Tells whether an offered harness needs Node.js: npm can't install it without Node, or the
 * resolver found it but it can't run without Node. Input: the versions read. Output: the answer. */
export function nodeNeeded(reads: readonly (HarnessVersions | null)[]): boolean {
  return reads.some((read) => read !== null && (read.refusal === NEEDS_NODE || read.needs?.program === "node"));
}

/** Tells whether the step reads again for Node while a harness needs it: setup's Node row is still
 * being checked, or it reads found or installed while the harness read still needs Node, as when it
 * was read just before Node was linked. Inputs: the need and the Node row's state, null while
 * unread. Output: the answer. */
export function nodeWaits(_needed: boolean, _node: DependencyState | null): boolean {
  return false;
}

/** What the step offers for Node.js, from whether a harness needs it and setup's Node row. A row
 * not settled while a setup run goes on reads installing, since setup finds Node before it marks
 * the row. Inputs: the need, the Node row's state, null while unread, and whether a setup run goes
 * on. Output: the offer. */
export function nodeOffer(needed: boolean, node: DependencyState | null, busy: boolean): NodeOffer {
  if (!needed) return { kind: "none" };
  switch (node?.state) {
    case "installing": return { kind: "installing" };
    case "failed": return { kind: "failed", reason: displayText(node.reason) };
    case "found":
    case "installed":
    case "bundled": return { kind: "none" };
    default:
      if (busy) return { kind: "installing" };
      return node === null || node.state === "missing" ? { kind: "offer" } : { kind: "none" };
  }
}
