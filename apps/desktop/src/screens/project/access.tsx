import { useState } from "react";
import type { AccessApply, AccessGrant, AgentAccess, AgentNetwork, AgentPolicy, GrantLogRow, IdeAgentFiles, PolicyView, ToolCacheScope } from "../../api-projects";
import { Switch } from "../../ui";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The plain warning the owner sees before giving a project's agents Full access. */
export const FULL_ACCESS_WARNING = "Agents run with your user's permissions: they can read your keys and change any file. Processes an agent starts outside its terminal can keep running after it stops.";

/** What Full access means for the IDE, the same in the TUI. */
export const FULL_ACCESS_IDE = "The IDE runs with your user's permissions. The project's network choice does not limit it. The AI lock and extension approvals are written at each IDE start, and anything inside the IDE can change them until the next one. Workspace trust is on. Until you trust the folder, IDE git and extensions that need trust stay off.";

/** What a lower saved level does to agents already running: the engine stops each one that started above it,
 * but not what a Full access agent started outside its terminal. */
export const LOWERING_STOPS_AGENTS = "Lowering the level stops agents running above it; they restart at the new level. It does not reach processes a Full access agent started outside its terminal.";

/** What saving a new level, hooks, agent folders or network choice does to the project's open IDE, the same in the TUI. */
export const POLICY_RESTARTS_IDE = "Changing the level, the IDE hooks switch or the agent folders choice below Full access, or the network choice at Own branch restarts this project's IDE at once. Its terminals end, and it reopens within a minute.";

/** What turning a toolchain off does to work already running: the engine stops each agent, research job and master turn its sandbox granted it to. */
export const TOOLCHAIN_OFF_STOPS_AGENTS = "Turning a toolchain off stops agents, research jobs and master turns running with it; their next start goes without it.";

/** The IDE hooks switch's name, the same in the TUI. */
export const IDE_GIT_HOOKS = "IDE git runs the project's hooks";

/** What the IDE hooks switch does and never does, the same in the TUI. */
export const IDE_GIT_HOOKS_LINE = "Off: git in the IDE skips this project's hooks. Agent folders never run theirs, and commits from the IDE are never signed. No effect at Full access: git runs as yours. Applies when the IDE next starts.";

/** Every access level from the most to the least confined, with what it lets agents do. */
export const ACCESS_LEVELS: { access: AgentAccess; label: string; help: string }[] = [
  { access: "sandboxed", label: "Sandboxed", help: "Edits only its own worktree and makes no git commits. Your keys and other folders stay out of reach." },
  { access: "own_branch", label: "Own branch", help: "Full git on its own branch in its own git folder. Its commits reach your branch only when you Accept." },
  { access: "full", label: "Full access", help: FULL_ACCESS_WARNING },
];

/** Names an access level as the owner sees it. Input: the level. Output: the label. */
export function accessLabel(access: AgentAccess): string {
  return ACCESS_LEVELS.find((level) => level.access === access)?.label ?? access;
}

/** Every network choice, widest first, with what it lets agents and the plugin check reach. */
export const NETWORK_CHOICES: { network: AgentNetwork; label: string; help: string }[] = [
  { network: "open", label: "Open", help: "Agents with tools on reach the whole network, and so does the plugin check." },
  { network: "https", label: "Port 443 only", help: "Agents and the plugin check reach port 443 only; agents also still reach the model." },
  { network: "none", label: "None", help: "None limits agents to their model: a hosted model's own hosts, through the engine's proxy, or only the local model's port. The plugin check reaches nothing." },
];

/** Why the network choice has no effect under Full access. */
export const NETWORK_IGNORED_UNDER_FULL = "Full access runs agents without a sandbox, so this choice does not apply.";

/** The agent folders choice's name, the same in the TUI. */
export const IDE_AGENT_FILES = "IDE changes in agent folders";

/** What each agent folders choice lets the IDE, its terminals and its tasks do in agent folders
 * at Sandboxed and Own branch (DEC-124), the same in the TUI. */
export const IDE_AGENT_FILES_CHOICES: { choice: IdeAgentFiles; label: string; help: string }[] = [
  { choice: "allow_all", label: "Allow all changes", help: "The IDE, its terminals and its tasks may edit, delete and rename files in agent folders." },
  { choice: "no_delete_or_rename", label: "No delete or rename", help: "Files in agent folders can be edited, Edit anyway included, but not deleted or renamed. Tools that delete as they go, such as cargo clean, fail there." },
  { choice: "read_only", label: "Read only", help: "Nothing in the IDE may write in agent folders, Edit anyway included." },
];

/** Why the agent folders choice has no effect under Full access, the same in the TUI. */
export const AGENT_FILES_IGNORED_UNDER_FULL = "Full access runs the IDE without a sandbox, so this choice does not apply.";

/** Names a network choice as the owner sees it. Input: the choice. Output: the label. */
export function networkLabel(network: AgentNetwork): string {
  return NETWORK_CHOICES.find((choice) => choice.network === network)?.label ?? network;
}

/** Every tool cache choice, the default first, with the engine's label and what it means
 * for the uv cache and pnpm store a sandboxed agent's tools write (T8). */
export const TOOL_CACHE_CHOICES: { scope: ToolCacheScope; label: string; help: string }[] = [
  { scope: "shared", label: "Shared by agents with the same access", help: "Agents at the same level working in the same kind of folder (the project folder, a worktree, their own copy or a lead's teammates) share one uv cache and pnpm store, so a package downloads once. One agent can change what the other agents at its level install." },
  { scope: "per_agent", label: "One per agent", help: "Each agent gets its own uv cache and pnpm store, so no agent sees what another downloaded. Each downloads its own packages." },
];

/** What an agent's uv cache clean does, and when CrazyAgent removes the caches. */
export const TOOL_CACHE_CLEAN = "uv cache clean fails inside an agent's sandbox. CrazyAgent removes a project's caches when you delete the project, and an agent's own cache when Clean up removes its worktree.";

/** Why the tool cache choice has no effect under Full access. */
export const TOOL_CACHE_IGNORED_UNDER_FULL = "Full access runs agents without a sandbox, so their tools use their own caches.";

/** The most extras of each kind a project allows, as the engine bounds them (F08). */
export const GRANT_SOCKETS_MAX = 2;
export const GRANT_PORTS_MAX = 4;
/** The highest port an extra may name: from 49152 up the system lends ports to the IDE and other apps. */
export const GRANT_PORT_MAX = 49_151;

/** Every Apply access changes choice, the default first, with what it does to running agents (F08). */
export const APPLY_CHOICES: { apply: AccessApply; label: string; help: string }[] = [
  { apply: "when_idle", label: "When the agent is idle", help: "A wider level, network, toolchain or extra stops each running agent at its next idle point so its next start has it. The master starts again in its own conversation." },
  { apply: "next_start", label: "At its next start", help: "Running agents keep the access they started with until they next start. Lowering the level or removing an extra still stops them at once." },
];

/** What removing an extra does to agents already running. */
export const REMOVING_EXTRA_STOPS_AGENTS = "Removing an extra stops every agent holding it at once; its next start goes without it.";

/** Why sockets and ports change nothing under Full access. */
export const EXTRAS_UNDER_FULL = "Full access runs agents without a sandbox, so sockets and ports are already open to them; only Your temp folder changes the temp folder they use.";

/** What each kind of extra lets agents do, in the owner's words, the same as the engine's question. */
export const GRANT_MEANINGS: Record<AccessGrant["kind"], string> = {
  socket: "Agents may connect to this socket and talk to the program behind it with your rights, but never replace or remove it. Container, VM, terminal, editor, key and password agent sockets, and sockets of other users, are never allowed.",
  port: "Agents may connect to this port on this computer, even when their network is limited. Ports from 49152 up, your sign-in, your local model and CrazyAgent's own ports are never allowed.",
  temp_folder: "Agents use your own temp folder as theirs: they can read and change other apps' temp files there, but no socket in it.",
};

/** Names one extra as the owner reads it, the same words the engine uses. Input: the extra. Output: the label. */
export function grantLabel(grant: AccessGrant): string {
  if (grant.kind === "socket") return `Socket ${grant.path}`;
  if (grant.kind === "port") return `Port ${grant.port} on this computer`;
  return "Your temp folder";
}

/** Whether two extras name the same thing. Inputs: the two extras. Output: the answer. */
export function sameGrant(a: AccessGrant, b: AccessGrant): boolean {
  return grantLabel(a) === grantLabel(b);
}

/** The saved extras of a view, none for an engine from before them. Input: the view. Output: the extras. */
export function savedGrants(view: PolicyView): AccessGrant[] {
  return (view.grants ?? []).map((row) => row.grant);
}

/** The policy Save sends: the level, the network choice, a choice only for each toolchain
 * whose switch differs from the project's own use, so the others keep following the project,
 * the IDE hooks switch, the agent folders choice, the tool cache choice, the Apply access
 * changes choice and the extras. Inputs: the engine's view, the chosen level and network, one
 * switch per view toolchain, the IDE hooks switch, the agent folders choice, and the tool
 * cache choice, Apply choice and extras, each the saved one when left out, so a save from
 * another screen never drops an extra. Output: the policy. */
export function policyOf(view: PolicyView, access: AgentAccess, network: AgentNetwork, allowed: boolean[], ideGitHooks: boolean, ideAgentFiles: IdeAgentFiles, toolCache: ToolCacheScope = view.tool_cache, applyChanges: AccessApply = view.apply_changes ?? "when_idle", grants: AccessGrant[] = savedGrants(view)): AgentPolicy {
  assert(allowed.length === view.toolchains.length, "one switch per toolchain");
  assert(grants.filter((grant) => grant.kind === "socket").length <= GRANT_SOCKETS_MAX, "at most 2 sockets");
  assert(grants.filter((grant) => grant.kind === "port").length <= GRANT_PORTS_MAX, "at most 4 ports");
  return {
    access,
    network,
    toolchains: view.toolchains.flatMap((row, index) => row.used === allowed[index] ? [] : [{ toolchain: row.toolchain, allowed: allowed[index] }]),
    ide_git_hooks: ideGitHooks,
    tool_cache: toolCache,
    ide_agent_files: ideAgentFiles,
    apply_changes: applyChanges,
    grants,
  };
}

/** Reads one extra the owner typed: a full socket path of plain characters, or a port from 1
 * to 49151. The engine checks the rest on save. Inputs: the kind and the typed text. Output:
 * the extra, or the plain reason it was not taken. */
export function typedGrant(kind: "socket" | "port", text: string): AccessGrant | string {
  const typed = text.trim();
  if (kind === "port") {
    const port = Number(typed);
    if (!/^[0-9]+$/.test(typed) || port < 1 || port > GRANT_PORT_MAX) return `Give a port from 1 to ${GRANT_PORT_MAX}.`;
    return { kind: "port", port };
  }
  if (!/^\/[A-Za-z0-9/._-]+$/.test(typed) || typed.length > 103 || typed.endsWith("/")) return "Give the socket's full path, of letters, digits, slashes, dots, dashes and underscores, at most 103 characters.";
  return { kind: "socket", path: typed };
}

/** One line on the project page for the saved policy: the level and the toolchains agents may use.
 * Input: the view. Output: the line. */
export function accessSummary(view: PolicyView): string {
  const tools = view.toolchains.filter((row) => row.allowed).map((row) => row.label);
  return `${accessLabel(view.access)} · ${tools.length ? tools.join(", ") : "no toolchains"}`;
}

/** Whether the owner must confirm the Full access warning before saving: on every save
 * with Full chosen, as the TUI asks, since the level the screen showed may no longer be the
 * saved one. Input: the chosen level. Output: true to ask first. */
export function confirmFirst(chosen: AgentAccess): boolean {
  return chosen === "full";
}

function toolchainNote(row: PolicyView["toolchains"][number]): string {
  if (row.used && row.installed) return "Used by this project.";
  if (row.used) return "Used by this project, not found in your home folder.";
  if (row.installed) return "Installed, not used by this project.";
  return "";
}

/** The access level choice as radio cards, with the reason a level can't run here and the Full
 * access warning when it is chosen. Inputs: the radio group name, the chosen level, the reason
 * each level is unavailable (null when it runs) and the change handler. Output: the fieldset. */
export function AccessLevels({ name, value, unavailable, onChange }: {
  name: string; value: AgentAccess; unavailable: (access: AgentAccess) => string | null; onChange: (access: AgentAccess) => void;
}) {
  return <fieldset className="pj-field">
    <legend>Agent access</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label="Agent access">
      {ACCESS_LEVELS.map((level) => {
        const reason = unavailable(level.access);
        return <label className={`pj-radio-card ${value === level.access ? "selected" : ""}`} key={level.access}>
          <input type="radio" name={name} value={level.access} checked={value === level.access} disabled={reason !== null} onChange={() => onChange(level.access)} />
          <span className="pj-radio-body">
            <strong>{level.label}</strong>
            <span className="pj-secondary">{level.help}</span>
            {reason && <span className="pj-small pj-muted">Unavailable: {reason}</span>}
          </span>
        </label>;
      })}
    </div>
    {value === "full" && <p className="pj-note warn" role="alert">{FULL_ACCESS_WARNING} {FULL_ACCESS_IDE}</p>}
  </fieldset>;
}

/** The network choice as radio cards, with a note when Full access makes it moot. Inputs: the
 * radio group name, the chosen network, the chosen access level and the change handler.
 * Output: the fieldset. */
export function NetworkChoices({ name, value, access, onChange }: {
  name: string; value: AgentNetwork; access: AgentAccess; onChange: (network: AgentNetwork) => void;
}) {
  return <fieldset className="pj-field">
    <legend>Network for agents with tools on</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label="Agent network">
      {NETWORK_CHOICES.map((choice) => <label className={`pj-radio-card ${value === choice.network ? "selected" : ""}`} key={choice.network}>
        <input type="radio" name={name} value={choice.network} checked={value === choice.network} onChange={() => onChange(choice.network)} />
        <span className="pj-radio-body">
          <strong>{choice.label}</strong>
          <span className="pj-secondary">{choice.help}</span>
        </span>
      </label>)}
    </div>
    {access === "full" && <p className="pj-note" role="note">{NETWORK_IGNORED_UNDER_FULL}</p>}
  </fieldset>;
}

/** The tool cache choice as radio cards, with what uv cache clean does and a note when Full
 * access makes it moot. Inputs: the radio group name, the chosen scope, the chosen access
 * level and the change handler. Output: the fieldset. */
export function ToolCacheChoices({ name, value, access, onChange }: {
  name: string; value: ToolCacheScope; access: AgentAccess; onChange: (scope: ToolCacheScope) => void;
}) {
  return <fieldset className="pj-field">
    <legend>Tool cache for uv and pnpm</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label="Agent tool cache">
      {TOOL_CACHE_CHOICES.map((choice) => <label className={`pj-radio-card ${value === choice.scope ? "selected" : ""}`} key={choice.scope}>
        <input type="radio" name={name} value={choice.scope} checked={value === choice.scope} onChange={() => onChange(choice.scope)} />
        <span className="pj-radio-body">
          <strong>{choice.label}</strong>
          <span className="pj-secondary">{choice.help}</span>
        </span>
      </label>)}
    </div>
    <p className="pj-help">{TOOL_CACHE_CLEAN}</p>
    {access === "full" && <p className="pj-note" role="note">{TOOL_CACHE_IGNORED_UNDER_FULL}</p>}
  </fieldset>;
}

/** The agent folders choice as radio cards, with a note when Full access makes it moot. Inputs:
 * the radio group name, the chosen choice, the chosen access level and the change handler.
 * Output: the fieldset. */
export function IdeAgentFilesChoices({ name, value, access, onChange }: {
  name: string; value: IdeAgentFiles; access: AgentAccess; onChange: (choice: IdeAgentFiles) => void;
}) {
  return <fieldset className="pj-field">
    <legend>{IDE_AGENT_FILES}</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label={IDE_AGENT_FILES}>
      {IDE_AGENT_FILES_CHOICES.map((choice) => <label className={`pj-radio-card ${value === choice.choice ? "selected" : ""}`} key={choice.choice}>
        <input type="radio" name={name} value={choice.choice} checked={value === choice.choice} onChange={() => onChange(choice.choice)} />
        <span className="pj-radio-body">
          <strong>{choice.label}</strong>
          <span className="pj-secondary">{choice.help}</span>
        </span>
      </label>)}
    </div>
    {access === "full" && <p className="pj-note" role="note">{AGENT_FILES_IGNORED_UNDER_FULL}</p>}
  </fieldset>;
}

/** One switch per detected toolchain family. Inputs: the id prefix, the engine's view, the
 * switches and the change handler. Output: the fieldset. */
export function ToolchainSwitches({ prefix, view, allowed, onChange }: {
  prefix: string; view: PolicyView; allowed: boolean[]; onChange: (allowed: boolean[]) => void;
}) {
  return <fieldset className="pj-field">
    <legend>Toolchains agents may use</legend>
    <p className="pj-help">Agents read these from your home folder and never change them or download into them. Homebrew tools stay readable.</p>
    {view.toolchains.length === 0 && <p className="pj-muted">No toolchains found in your home folder or this project.</p>}
    <div className="pj-switch-list">{view.toolchains.map((row, index) => <div className="pj-switch-item" key={row.toolchain}>
      <span><strong>{row.label}</strong><span className="pj-secondary">{toolchainNote(row)}</span></span>
      <Switch id={`${prefix}-toolchain-${row.toolchain}`} label={row.label} hideLabel checked={allowed[index]}
        onChecked={(on) => onChange(allowed.map((value, at) => at === index ? on : value))} />
    </div>)}</div>
  </fieldset>;
}

/** The IDE hooks switch with what it does and never does. Inputs: the id prefix, the switch and
 * the change handler. Output: the fieldset. */
export function IdeGitHooksSwitch({ prefix, checked, onChange }: {
  prefix: string; checked: boolean; onChange: (checked: boolean) => void;
}) {
  return <fieldset className="pj-field">
    <legend>IDE git</legend>
    <div className="pj-switch-list"><div className="pj-switch-item">
      <span><strong>{IDE_GIT_HOOKS}</strong><span className="pj-secondary">{IDE_GIT_HOOKS_LINE}</span></span>
      <Switch id={`${prefix}-ide-git-hooks`} label={IDE_GIT_HOOKS} hideLabel checked={checked} onChecked={onChange} />
    </div></div>
  </fieldset>;
}

/** The Apply access changes choice as radio cards (F08). Inputs: the radio group name, the
 * chosen choice and the change handler. Output: the fieldset. */
export function ApplyChoices({ name, value, onChange }: {
  name: string; value: AccessApply; onChange: (apply: AccessApply) => void;
}) {
  return <fieldset className="pj-field">
    <legend>Apply access changes</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label="Apply access changes">
      {APPLY_CHOICES.map((choice) => <label className={`pj-radio-card ${value === choice.apply ? "selected" : ""}`} key={choice.apply}>
        <input type="radio" name={name} value={choice.apply} checked={value === choice.apply} onChange={() => onChange(choice.apply)} />
        <span className="pj-radio-body">
          <strong>{choice.label}</strong>
          <span className="pj-secondary">{choice.help}</span>
        </span>
      </label>)}
    </div>
  </fieldset>;
}

/** The allowed extras: each saved or added one with what it means and Remove, and a form to
 * add one socket, one port or the owner's temp folder within their bounds (F08). Inputs: the id
 * prefix, the engine's view, the extras as edited, the chosen level and the change handler.
 * Output: the fieldset. */
export function GrantsEditor({ prefix, view, grants, access, onChange }: {
  prefix: string; view: PolicyView; grants: AccessGrant[]; access: AgentAccess; onChange: (grants: AccessGrant[]) => void;
}) {
  const [kind, setKind] = useState<"socket" | "port">("port");
  const [typed, setTyped] = useState("");
  const [problem, setProblem] = useState("");
  const sockets = grants.filter((grant) => grant.kind === "socket").length;
  const ports = grants.filter((grant) => grant.kind === "port").length;
  const full = kind === "socket" ? sockets >= GRANT_SOCKETS_MAX : ports >= GRANT_PORTS_MAX;
  const temp = view.temp_folder ?? { path: null, unavailable: null };
  const tempHeld = grants.some((grant) => grant.kind === "temp_folder");
  function add(grant: AccessGrant | string) {
    if (typeof grant === "string") { setProblem(grant); return; }
    if (grants.some((held) => sameGrant(held, grant))) { setProblem("That extra is already allowed."); return; }
    setProblem("");
    setTyped("");
    onChange([...grants, grant]);
  }
  return <fieldset className="pj-field">
    <legend>Allowed extras</legend>
    <p className="pj-help">One socket, one port on this computer or your temp folder that agents may reach beyond their level. Agents can also ask for one; you answer in Questions.</p>
    <ul className="pj-paths" aria-label="Allowed extras">
      {grants.length === 0 && <li className="pj-muted">No extras.</li>}
      {grants.map((grant) => <li key={grantLabel(grant)}>
        <span className="pj-mono">{grantLabel(grant)}</span>{" "}
        <button type="button" className="btn" aria-label={`Remove ${grantLabel(grant)}`} onClick={() => onChange(grants.filter((held) => !sameGrant(held, grant)))}>Remove</button>
        <span className="pj-secondary">{GRANT_MEANINGS[grant.kind]}</span>
      </li>)}
    </ul>
    <form className="pj-field" onSubmit={(event) => { event.preventDefault(); add(typedGrant(kind, typed)); }}>
      <div className="pj-radio-cards" role="radiogroup" aria-label="Kind of extra">
        {(["port", "socket"] as const).map((option) => <label className={`pj-radio-card ${kind === option ? "selected" : ""}`} key={option}>
          <input type="radio" name={`${prefix}-grant-kind`} value={option} checked={kind === option} onChange={() => { setKind(option); setProblem(""); }} />
          <span className="pj-radio-body"><strong>{option === "port" ? "Port" : "Socket"}</strong><span className="pj-secondary">{GRANT_MEANINGS[option]}</span></span>
        </label>)}
      </div>
      <label className="pj-label" htmlFor={`${prefix}-grant-value`}>{kind === "port" ? "Port" : "Socket path"}</label>
      <input id={`${prefix}-grant-value`} className="pj-input" value={typed} inputMode={kind === "port" ? "numeric" : "text"}
        placeholder={kind === "port" ? "5432" : "/Users/you/.local/run/app.sock"} onChange={(event) => setTyped(event.target.value)} />
      <div><button type="submit" className="btn" disabled={!typed.trim() || full}>{kind === "port" ? "Add port" : "Add socket"}</button></div>
      {full && <p className="pj-muted">{kind === "port" ? `At most ${GRANT_PORTS_MAX} ports.` : `At most ${GRANT_SOCKETS_MAX} sockets.`}</p>}
    </form>
    <div className="pj-switch-list"><div className="pj-switch-item">
      <span><strong>Your temp folder</strong><span className="pj-secondary">{temp.unavailable ?? `${GRANT_MEANINGS.temp_folder} ${temp.path ?? ""}`}</span></span>
      <button type="button" className="btn" id={`${prefix}-grant-temp`} disabled={tempHeld || temp.unavailable !== null} onClick={() => add({ kind: "temp_folder" })}>Allow temp folder</button>
    </div></div>
    {problem && <p className="pj-note warn" role="alert">{problem}</p>}
    <p className="pj-help">{REMOVING_EXTRA_STOPS_AGENTS}</p>
    {access === "full" && <p className="pj-note" role="note">{EXTRAS_UNDER_FULL}</p>}
  </fieldset>;
}

/** The extras log, newest first: who granted, revoked, allowed or denied each and when. Input:
 * the log rows. Output: the list, or nothing for an empty log. */
export function GrantLog({ rows }: { rows: GrantLogRow[] }) {
  if (rows.length === 0) return null;
  const said: Record<GrantLogRow["action"], string> = { granted: "Allowed", revoked: "Removed", allowed: "Allowed on request", denied: "Not allowed on request" };
  return <section className="pj-field" aria-label="Extras log">
    <h3>Extras log</h3>
    <ul className="pj-paths">{rows.map((row, index) => <li key={`${row.at_ms}-${index}`}>
      <span className="pj-muted">{new Date(row.at_ms).toLocaleString()}</span>{" "}
      <strong>{said[row.action]}</strong>{" "}<span className="pj-mono">{row.grant}</span>{" "}
      <span className="pj-secondary">{row.by === "owner" ? "by you" : `by your answer to ${row.by}`}</span>
    </li>)}</ul>
  </section>;
}
