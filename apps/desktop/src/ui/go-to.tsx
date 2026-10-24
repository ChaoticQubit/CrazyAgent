import { useEffect, useId, useRef, useState } from "react";
import { Dialog } from "./overlay";
import { formatRoute, navigate, type Route } from "./router";

export const GO_TO_ROWS_MAX = 8;
export const GO_TO_QUERY_MAX = 100;

const PROJECT_TAB_LABELS = [
  ["overview", "Overview"], ["conversation", "Conversation"], ["agents", "Agents"], ["context", "Context"], ["settings", "Settings"],
] as const;
const SETTINGS_LABELS = [
  ["accounts", "Accounts"], ["pools", "Pools"], ["harnesses", "Harnesses"], ["plugins", "Plugins and skills"], ["memory", "Memory"], ["profile", "Profile"],
] as const;

/** One Go to destination: its label, what kind of place it is, and the route. */
export interface Target { label: string; kind: "Project" | "Project tab" | "Place" | "Settings" | "Action"; route: Route }

/** The shortcuts the shortcuts dialog lists, in order. */
export const SHORTCUTS: readonly (readonly [string, string])[] = [
  ["Cmd+K", "Go to anything"],
  ["Cmd+/", "Keyboard shortcuts"],
  ["Cmd+N", "New project"],
  ["Cmd+,", "Settings"],
  ["Cmd+[  Cmd+]", "Back, forward"],
  ["Cmd+1 to Cmd+5", "Project tabs"],
  ["Cmd+Enter", "Send a message or prompt"],
  ["Esc", "Close a dialog or panel"],
  ["Arrow keys", "Move a focused box in the team graph"],
];

/** Lists every Go to destination, like the TUI: each project and its tabs, Home, every
 * Settings section, then the actions. Input: the projects. Output: targets in display order. */
export function goToTargets(projects: readonly { id: string; name: string }[]): Target[] {
  const out: Target[] = [];
  for (const project of projects) {
    for (const [tab, label] of PROJECT_TAB_LABELS) {
      out.push({
        label: tab === "overview" ? project.name : `${project.name} / ${label}`,
        kind: tab === "overview" ? "Project" : "Project tab",
        route: { name: "project", id: project.id, tab },
      });
    }
  }
  out.push({ label: "Home", kind: "Place", route: { name: "home" } });
  for (const [tab, label] of SETTINGS_LABELS) out.push({ label: `Settings / ${label}`, kind: "Settings", route: { name: "settings", tab } });
  out.push(
    { label: "New project", kind: "Action", route: { name: "new-project", step: "basics" } },
    { label: "Clone from GitHub", kind: "Action", route: { name: "add-project", way: "clone" } },
    { label: "Add existing folder", kind: "Action", route: { name: "add-project", way: "folder" } },
    { label: "Add account", kind: "Action", route: { name: "settings", tab: "accounts", action: "add" } },
    { label: "New pool", kind: "Action", route: { name: "settings", tab: "pools", action: "new" } },
  );
  return out;
}

/** Ranks a Go to query against a label the way the TUI does: 0 for a prefix (or an empty
 * query), 1 when contained, 2 when the letters appear in order, null otherwise; case is
 * ignored. Inputs: the query and the label. Output: the rank. */
export function rank(query: string, label: string): number | null {
  const wanted = query.trim().toLowerCase();
  const text = label.toLowerCase();
  if (!wanted || text.startsWith(wanted)) return 0;
  if (text.includes(wanted)) return 1;
  let at = 0;
  for (const letter of wanted.replace(/\s+/g, "")) {
    at = text.indexOf(letter, at);
    if (at < 0) return null;
    at += 1;
  }
  return 2;
}

/** The best Go to matches: by rank, then display order, at most `GO_TO_ROWS_MAX`.
 * Inputs: the targets and the query. Output: the matching targets. */
export function matchTargets(targets: readonly Target[], query: string): Target[] {
  return targets
    .map((target, index) => ({ target, index, rank: rank(query.slice(0, GO_TO_QUERY_MAX), target.label) }))
    .filter((item): item is { target: Target; index: number; rank: number } => item.rank !== null)
    .sort((a, b) => a.rank - b.rank || a.index - b.index)
    .slice(0, GO_TO_ROWS_MAX)
    .map((item) => item.target);
}

/** The Go to dialog as a pure view: a search box driving a list of places, arrow keys to move,
 * Enter to go. Inputs: whether it is open, the projects, the go and close actions.
 * Output: the dialog. */
export function GoToDialog({ open, projects, onGo, onClose }: {
  open: boolean;
  projects: readonly { id: string; name: string }[];
  onGo: (route: Route) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const ids = useId();
  const matches = matchTargets(goToTargets(projects), query);
  const current = Math.min(selected, Math.max(0, matches.length - 1));
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setSelected(0);
    window.setTimeout(() => input.current?.focus(), 0);
  }, [open]);
  const go = (target: Target | undefined) => { if (target) onGo(target.route); };
  return <Dialog open={open} title="Go to" onClose={onClose}>
    <div className="go-to">
      <label htmlFor={`${ids}-query`} className="visually-hidden">Go to</label>
      <input ref={input} id={`${ids}-query`} className="input" type="search" role="combobox" aria-expanded="true" aria-controls={`${ids}-list`}
        aria-activedescendant={matches.length ? `${ids}-${current}` : undefined} autoComplete="off" spellCheck={false}
        maxLength={GO_TO_QUERY_MAX} placeholder="Projects, tabs, settings, actions" value={query}
        onChange={(event) => { setQuery(event.target.value); setSelected(0); }}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") { event.preventDefault(); setSelected(Math.min(current + 1, matches.length - 1)); }
          else if (event.key === "ArrowUp") { event.preventDefault(); setSelected(Math.max(current - 1, 0)); }
          else if (event.key === "Enter") { event.preventDefault(); go(matches[current]); }
        }} />
      {matches.length ? <ul id={`${ids}-list`} className="go-to-list" role="listbox" aria-label="Places">
        {matches.map((target, index) => <li key={formatRoute(target.route) + target.label} id={`${ids}-${index}`} role="option" aria-selected={index === current}
          className="go-to-item" onMouseEnter={() => setSelected(index)} onClick={() => go(target)}>
          <span className="go-to-label">{target.label}</span><span className="go-to-kind">{target.kind}</span>
        </li>)}
      </ul> : <p id={`${ids}-list`} className="secondary" role="status">Nothing matches "{query.trim()}".</p>}
    </div>
  </Dialog>;
}

/** The keyboard shortcuts dialog. Inputs: whether it is open and the close action. */
export function ShortcutsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  return <Dialog open={open} title="Keyboard shortcuts" onClose={onClose}>
    <dl className="shortcuts">
      {SHORTCUTS.map(([keys, what]) => <div key={keys} className="shortcut"><dt><kbd>{keys}</kbd></dt><dd>{what}</dd></div>)}
    </dl>
  </Dialog>;
}

/** Go to and the shortcuts list for the whole app: Cmd+K opens Go to, Cmd+/ the shortcuts.
 * Input: the projects Go to lists. Output: the two dialogs, closed until asked for. */
export function GoToLayer({ projects }: { projects: readonly { id: string; name: string }[] }) {
  const [open, setOpen] = useState<"go-to" | "shortcuts" | null>(null);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!event.metaKey || event.altKey || event.ctrlKey) return;
      if (event.key === "k") { event.preventDefault(); setOpen("go-to"); }
      else if (event.key === "/") { event.preventDefault(); setOpen("shortcuts"); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return <>
    <GoToDialog open={open === "go-to"} projects={projects} onClose={() => setOpen(null)}
      onGo={(route) => { setOpen(null); navigate(route); }} />
    <ShortcutsDialog open={open === "shortcuts"} onClose={() => setOpen(null)} />
  </>;
}
