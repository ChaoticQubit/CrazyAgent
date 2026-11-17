import { useEffect, useId, useRef, useState } from "react";
import { keyLabel, shortcutKey } from "./keys";
import { Dialog } from "./overlay";
import { formatRoute, navigate, type Route } from "./router";
import { loadTeamStatus } from "../api-projects";
import type { TeamStatus } from "../engine-client";
import { IDE_TAB, TABS } from "../screens/project/index";
import { useProjectFiles } from "../screens/project/project-files";
import { agentKey, liveAgent } from "../screens/project/research";
import { agentSection, fileSection } from "../screens/project/shared";

export const GO_TO_ROWS_MAX = 8;
export const GO_TO_QUERY_MAX = 100;

const SETTINGS_LABELS = [
  ["accounts", "Accounts"], ["pools", "Pools"], ["harnesses", "Harnesses"], ["plugins", "Plugins and skills"], ["memory", "Memory"],
] as const;

/** One Go to destination: its label, what kind of place it is, and the route. */
export interface Target { label: string; kind: "Project" | "Project tab" | "Agent" | "File" | "Place" | "Settings" | "Action"; route: Route }

/** One running agent Go to lists: its Team view row key and its name. */
export interface GoToAgent { key: string; label: string }

/** The open project's running agents and files, as Go to lists them. */
export interface OpenProject { id: string; agents: readonly GoToAgent[]; files: readonly string[] }

/** The shortcuts the shortcuts dialog lists, in order. */
export const SHORTCUTS: readonly (readonly [string, string])[] = [
  ["Cmd+K", "Go to anything"],
  ["Cmd+/", "Keyboard shortcuts"],
  ["Cmd+N", "New project"],
  ["Cmd+,", "Settings"],
  ["Cmd+[  Cmd+]", "Back, forward"],
  ["Cmd+1  Cmd+2  ...", "Project tabs, in the order shown"],
  ["Enter", "Send a message or prompt"],
  ["Shift+Enter", "New line in a message or prompt"],
  ["Esc", "Close a dialog or panel"],
  ["Arrow keys", "Move a focused box in the team graph"],
];

/** Lists every Go to destination, like the TUI: each project with every project page tab
 * plus the IDE, the open project's running agents and files, Home, Profile (a dialog from the
 * user menu since DEC-151), every Settings section, then the actions. Inputs: the projects and
 * the open project, if any. Output: targets in display order. */
export function goToTargets(projects: readonly { id: string; name: string }[], open?: OpenProject): Target[] {
  const out: Target[] = [];
  for (const project of projects) {
    for (const { id: tab, label } of [...TABS, IDE_TAB]) {
      out.push({
        label: tab === "overview" ? project.name : `${project.name} / ${label}`,
        kind: tab === "overview" ? "Project" : "Project tab",
        route: { name: "project", id: project.id, tab },
      });
    }
  }
  if (open) {
    for (const agent of open.agents) {
      out.push({ label: agent.label, kind: "Agent", route: { name: "project", id: open.id, tab: "agents", section: agentSection(agent.key) } });
    }
    for (const path of open.files) {
      out.push({ label: path, kind: "File", route: { name: "project", id: open.id, tab: "ide", section: fileSection(path) } });
    }
  }
  out.push({ label: "Home", kind: "Place", route: { name: "home" } }, { label: "Profile", kind: "Place", route: { name: "settings", tab: "profile" } });
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

/** The running sub-agents of a project that the Team view shows with a Messages row: live
 * ones, and finished ones only while the project shows finished agents. Input: the team
 * status. Output: each agent's row key and name, in engine order. */
export function goToAgents(status: Pick<TeamStatus, "agents" | "show_finished">): GoToAgent[] {
  return status.agents
    .filter((agent) => agent.role === "sub" && liveAgent(agent.status) && (status.show_finished === true || agent.finished !== true))
    .map((agent) => ({ key: agentKey(agent.id), label: agent.label }));
}

/** Reads a project's running agents once each time Go to opens on it, and none while closed.
 * Input: the open project's id, or null while Go to is closed or no project is open.
 * Output: the agents, empty until read or when the read fails. */
function useOpenAgents(projectId: string | null): readonly GoToAgent[] {
  const [agents, setAgents] = useState<readonly GoToAgent[]>([]);
  useEffect(() => {
    setAgents([]);
    if (projectId === null) return;
    let live = true;
    void loadTeamStatus(projectId).then((result) => {
      if (live && result.kind === "ok") setAgents(goToAgents(result.value));
    });
    return () => { live = false; };
  }, [projectId]);
  return agents;
}

/** The Go to dialog: a search box driving a list of places, arrow keys to move, Enter to go.
 * While open on a project page it also lists that project's running agents and its files,
 * read only while the dialog is open. Inputs: whether it is open, the projects, the open
 * project's id, if any, and the go and close actions. Output: the dialog. */
export function GoToDialog({ open, projects, project = null, onGo, onClose }: {
  open: boolean;
  projects: readonly { id: string; name: string }[];
  project?: string | null;
  onGo: (route: Route) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const ids = useId();
  const files = useProjectFiles(project ?? "", open && project !== null ? query : null);
  const agents = useOpenAgents(open ? project : null);
  const here = project === null ? undefined : { id: project, agents, files: files ?? [] };
  const matches = matchTargets(goToTargets(projects, here), query);
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
        maxLength={GO_TO_QUERY_MAX} placeholder="Projects, tabs, agents, files, settings, actions" value={query}
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
      {SHORTCUTS.map(([keys, what]) => <div key={keys} className="shortcut"><dt><kbd>{keyLabel(keys)}</kbd></dt><dd>{what}</dd></div>)}
    </dl>
  </Dialog>;
}

/** Go to and the shortcuts list for the whole app: Cmd+K opens Go to, Cmd+/ the shortcuts
 * (Ctrl on Windows and Linux). Inputs: the projects Go to lists and the open project's id,
 * if any. Output: the two dialogs, closed until asked for. */
export function GoToLayer({ projects, project = null }: { projects: readonly { id: string; name: string }[]; project?: string | null }) {
  const [open, setOpen] = useState<"go-to" | "shortcuts" | null>(null);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!shortcutKey(event)) return;
      if (event.key === "k") { event.preventDefault(); setOpen("go-to"); }
      else if (event.key === "/") { event.preventDefault(); setOpen("shortcuts"); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return <>
    <GoToDialog open={open === "go-to"} projects={projects} project={project} onClose={() => setOpen(null)}
      onGo={(route) => { setOpen(null); navigate(route); }} />
    <ShortcutsDialog open={open === "shortcuts"} onClose={() => setOpen(null)} />
  </>;
}
