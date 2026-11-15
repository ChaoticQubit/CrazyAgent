import { useState } from "react";
import type { AccessApply, AccessGrant, AgentAccess, AgentNetwork, AgentPolicy, GrantLogRow, IdeAgentFiles, PolicyView, SharedAccess, SharedFolders, SharedScope, ToolCacheScope } from "../../api-projects";
import { desktopPlatform } from "../../api-ide";
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

/** What a limited network can't hold on Linux, shown there only, the engine's LINUX_PORTS_ANY_HOST_LINE. */
export const LINUX_PORTS_ANY_HOST = "On Linux, network limits hold ports, not hosts: a port agents may reach on this computer, such as the model's proxy port or a port you allow, is open to them on every host.";

/** What a writable project folder can't hold on Linux, shown there only, the engine's LINUX_NESTED_GIT_LINE. */
export const LINUX_NESTED_GIT = "On Linux, agents can make a git repository inside any project folder they may change, and your own git or IDE runs its settings when you open that folder. Check agent-made folders for a .git before you open them.";

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
  socket: "Agents may connect to this socket and talk to the program behind it with your rights, but never replace or remove it. If that program can run commands or change files (a database, a debugger, an editor, a terminal or a remote shell), agents can then do anything you can, outside their sandbox. Container, VM, terminal, editor, SSH, key and password agent sockets, and sockets of other users, are never allowed, but other programs can be just as powerful.",
  port: "Agents may connect to this port on this computer, even when their network is limited, and talk to the program behind it with your rights. If that program can run commands or change files (a database, a debugger, an editor, a terminal or a remote shell), agents can then do anything you can, outside their sandbox. Ports from 49152 up, your sign-in, your local model, Docker, debugger and CrazyAgent's own ports are never allowed, but other programs can be just as powerful.",
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

/** The most top-level folders Chosen folders may pick and the most secret paths the owner may
 * share back, the engine's CHOSEN_MAX and SECRETS_MAX. */
export const SHARED_CHOSEN_MAX = 16;
export const SHARED_SECRETS_MAX = 16;

/** The shared folders choice of a policy saved before the setting existed. */
export const DEFAULT_SHARED_FOLDERS: SharedFolders = { scope: "all", access: "read_only", chosen: [], secrets: [] };

/** The shared folders setting's name, the same in the TUI. */
export const SHARED_FOLDERS = "Shared project folders";

/** Every shared folders choice, the default first, with the engine's labels and what each
 * gives agents (DEC-147.9), the same in the TUI. */
export const SHARED_SCOPE_CHOICES: { scope: SharedScope; label: string; help: string }[] = [
  { scope: "all", label: "All local folders", help: "Agents see every folder in the project folder, the ones git ignores included, such as data and programs." },
  { scope: "chosen", label: "Chosen folders", help: "Agents see only the top-level folders you pick." },
  { scope: "none", label: "None", help: "Agents see only their own worktree." },
];

/** Every shared folders access choice, the default first, with the engine's labels and what
 * each lets agents do, the same in the TUI. */
export const SHARED_ACCESS_CHOICES: { access: SharedAccess; label: string; help: string }[] = [
  { access: "read_only", label: "Read-only", help: "Agents read shared folders and never change them." },
  { access: "read_write", label: "Read-write", help: "Agents may also change files in shared top-level folders git does not track, but not in hidden folders such as .venv or .vscode. Folders such as node_modules, target and venv stay writable, and your own tools run what is in them outside the sandbox. Folders inside stay in place: agents can't move or delete them. Tracked files change only in their worktree." },
];

/** What stays closed in shared folders, the same in the TUI. */
export const SHARED_SECRETS_LINE = "Secret files stay closed to agents wherever they sit: .env files, keys, certificates and credential files. Their names stay listed.";

/** How the owner shares a secret back for reading. */
export const SHARED_SECRETS_HOW = "To let agents read one, add its path inside the project folder, one per line.";

/** The line before the shared-back secrets the choice cannot reach, the engine's UNREACHABLE_SECRETS_LINE. */
export const SHARED_SECRETS_UNREACHABLE = "Still closed unless git tracks them, since this choice does not share their folder:";

/** What the Linux and Windows sandboxes can and can't do with this setting, the engine's OFF_MACOS_LINE. */
export const SHARED_FOLDERS_OFF_MACOS = "On Linux and Windows agents read the whole project folder, secret files included, and only All with Read-only can start them.";

/** When a changed shared folders choice reaches agents, the same in the TUI. */
export const SHARED_FOLDERS_NEXT_START = "A change reaches each agent, research job and master turn when it next starts.";

/** Why the shared folders choice has no effect under Full access, the same in the TUI. */
export const SHARED_FOLDERS_IGNORED_UNDER_FULL = "Full access runs agents without a sandbox, so they see the whole project folder, secret files included.";

/** The secret paths the owner typed, one per line, trimmed, blank lines and repeats dropped.
 * Input: the text. Output: the paths, in the order typed. */
export function sharedSecretsOf(text: string): string[] {
  const paths = text.split("\n").map((line) => line.trim()).filter((line) => line !== "");
  return paths.filter((path, index) => paths.indexOf(path) === index);
}

/** The shared-back secrets a choice cannot reach, as the engine's unreachable_secrets: under
 * None each one, under Chosen folders each one whose top-level folder is not picked. Inputs:
 * the scope, the picked folders and the secret paths. Output: the paths, in the order given. */
export function unreachableSecrets(scope: SharedScope, chosen: string[], secrets: string[]): string[] {
  if (scope === "all") return [];
  return secrets.filter((path) => scope === "none" || !chosen.includes(path.split("/")[0]));
}

/** The top-level folders the owner may pick: the ones the engine found and any picked before
 * that is gone now, so it can still be unpicked. Inputs: the engine's folders and the picked
 * ones. Output: the names, sorted. */
export function pickableFolders(local: string[], chosen: string[]): string[] {
  return [...new Set([...local, ...chosen])].sort();
}

/** Whether two shared folders choices save the same. Inputs: both choices. Output: the answer. */
export function sameShared(left: SharedFolders, right: SharedFolders): boolean {
  const same = (a: string[], b: string[]) => a.length === b.length && a.every((item, index) => item === b[index]);
  return left.scope === right.scope && left.access === right.access && same(left.chosen, right.chosen) && same(left.secrets, right.secrets);
}

/** The policy Save sends: the level, the network choice, a choice only for each toolchain
 * whose switch differs from the project's own use, so the others keep following the project,
 * the IDE hooks switch, the agent folders choice, the tool cache choice, the shared folders
 * choice, the Apply access changes choice and the extras. Inputs: the engine's view, the
 * chosen level and network, one switch per view toolchain, the IDE hooks switch, the agent
 * folders choice, and the tool cache, shared folders and Apply choices and the extras, each
 * the saved one when left out, so a save from another screen never drops an extra. Output:
 * the policy. */
export function policyOf(view: PolicyView, access: AgentAccess, network: AgentNetwork, allowed: boolean[], ideGitHooks: boolean, ideAgentFiles: IdeAgentFiles, toolCache: ToolCacheScope = view.tool_cache, sharedFolders: SharedFolders = view.shared_folders ?? DEFAULT_SHARED_FOLDERS, applyChanges: AccessApply = view.apply_changes ?? "when_idle", grants: AccessGrant[] = savedGrants(view)): AgentPolicy {
  assert(allowed.length === view.toolchains.length, "one switch per toolchain");
  assert(sharedFolders.chosen.length <= SHARED_CHOSEN_MAX && sharedFolders.secrets.length <= SHARED_SECRETS_MAX, "shared folders within the engine's bounds");
  assert(grants.filter((grant) => grant.kind === "socket").length <= GRANT_SOCKETS_MAX, "at most 2 sockets");
  assert(grants.filter((grant) => grant.kind === "port").length <= GRANT_PORTS_MAX, "at most 4 ports");
  return {
    access,
    network,
    toolchains: view.toolchains.flatMap((row, index) => row.used === allowed[index] ? [] : [{ toolchain: row.toolchain, allowed: allowed[index] }]),
    ide_git_hooks: ideGitHooks,
    tool_cache: toolCache,
    ide_agent_files: ideAgentFiles,
    shared_folders: sharedFolders,
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

/** The access level choice as radio cards, with the reason a level can't run here, the Full
 * access warning when it is chosen and, on Linux below Full access, the nested git gap. Inputs:
 * the radio group name, the chosen level, the reason each level is unavailable (null when it
 * runs), the change handler and this desktop's platform. Output: the fieldset. */
export function AccessLevels({ name, value, unavailable, onChange, platform = desktopPlatform() }: {
  name: string; value: AgentAccess; unavailable: (access: AgentAccess) => string | null; onChange: (access: AgentAccess) => void; platform?: string;
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
    {value !== "full" && platform === "linux" && <p className="pj-note" role="note">{LINUX_NESTED_GIT}</p>}
  </fieldset>;
}

/** The network choice as radio cards, with a note when Full access makes it moot or, on Linux,
 * that limits hold ports, not hosts. Inputs: the radio group name, the chosen network, the
 * chosen access level, the change handler and this desktop's platform. Output: the fieldset. */
export function NetworkChoices({ name, value, access, onChange, platform = desktopPlatform() }: {
  name: string; value: AgentNetwork; access: AgentAccess; onChange: (network: AgentNetwork) => void; platform?: string;
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
    {access !== "full" && platform === "linux" && <p className="pj-note" role="note">{LINUX_PORTS_ANY_HOST}</p>}
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

/** The shared folders choice: which local folders agents see as radio cards, the top-level
 * folders to pick for Chosen folders, Read-only or Read-write, and the secret paths shared
 * back, with what stays closed and the shared-back ones the choice cannot reach, the Linux
 * and Windows gap, when a change applies and a note when Full access makes it moot.
 * Inputs: the id prefix, the choice, the secret paths text, the engine's top-level folders,
 * the chosen access level and the change handlers. Output: the fieldset. */
export function SharedFoldersChoices({ prefix, value, secretsText, local, access, onChange, onSecretsText }: {
  prefix: string; value: SharedFolders; secretsText: string; local: string[]; access: AgentAccess;
  onChange: (value: SharedFolders) => void; onSecretsText: (text: string) => void;
}) {
  const folders = pickableFolders(local, value.chosen);
  const full = value.chosen.length >= SHARED_CHOSEN_MAX;
  const unreachable = unreachableSecrets(value.scope, value.chosen, sharedSecretsOf(secretsText));
  return <fieldset className="pj-field">
    <legend>{SHARED_FOLDERS}</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label={SHARED_FOLDERS}>
      {SHARED_SCOPE_CHOICES.map((choice) => <label className={`pj-radio-card ${value.scope === choice.scope ? "selected" : ""}`} key={choice.scope}>
        <input type="radio" name={`${prefix}-shared-scope`} value={choice.scope} checked={value.scope === choice.scope} onChange={() => onChange({ ...value, scope: choice.scope })} />
        <span className="pj-radio-body">
          <strong>{choice.label}</strong>
          <span className="pj-secondary">{choice.help}</span>
        </span>
      </label>)}
    </div>
    {value.scope === "chosen" && <div className="pj-field">
      <span className="pj-small pj-muted">Top-level folders to share, at most {SHARED_CHOSEN_MAX}</span>
      {folders.length === 0 && <p className="pj-muted">This project folder has no top-level folders to pick.</p>}
      <div className="pj-chips">{folders.map((name) => {
        const on = value.chosen.includes(name);
        return <label className={`pj-chip-toggle ${on ? "on" : ""}`} key={name}>
          <input type="checkbox" className="pj-sr" checked={on} disabled={!on && full}
            onChange={() => onChange({ ...value, chosen: on ? value.chosen.filter((item) => item !== name) : [...value.chosen, name] })} />{name}</label>;
      })}</div>
    </div>}
    <div className="pj-radio-cards" role="radiogroup" aria-label="Shared folders access">
      {SHARED_ACCESS_CHOICES.map((choice) => <label className={`pj-radio-card ${value.access === choice.access ? "selected" : ""}`} key={choice.access}>
        <input type="radio" name={`${prefix}-shared-access`} value={choice.access} checked={value.access === choice.access} onChange={() => onChange({ ...value, access: choice.access })} />
        <span className="pj-radio-body">
          <strong>{choice.label}</strong>
          <span className="pj-secondary">{choice.help}</span>
        </span>
      </label>)}
    </div>
    <label htmlFor={`${prefix}-shared-secrets`}>Secret files agents may read</label>
    <p className="pj-help" id={`${prefix}-shared-secrets-help`}>{SHARED_SECRETS_LINE} {SHARED_SECRETS_HOW}</p>
    <textarea id={`${prefix}-shared-secrets`} className="pj-input" rows={3} value={secretsText} spellCheck={false} aria-describedby={`${prefix}-shared-secrets-help`}
      onChange={(event) => onSecretsText(event.target.value)} />
    {sharedSecretsOf(secretsText).length > SHARED_SECRETS_MAX && <p className="pj-error" role="alert">At most {SHARED_SECRETS_MAX} secret paths.</p>}
    {unreachable.length > 0 && <p className="pj-note" role="note">{SHARED_SECRETS_UNREACHABLE} {unreachable.join(", ")}.</p>}
    <p className="pj-help">{SHARED_FOLDERS_NEXT_START} {SHARED_FOLDERS_OFF_MACOS}</p>
    {access === "full" && <p className="pj-note" role="note">{SHARED_FOLDERS_IGNORED_UNDER_FULL}</p>}
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

/** The allowed extras: each saved or added one with what it means and Remove, a form to add one
 * socket, one port or the owner's temp folder within their bounds (F08) and, on Linux below Full
 * access, that a port is open on every host. Inputs: the id prefix, the engine's view, the extras
 * as edited, the chosen level, the change handler and this desktop's platform. Output: the fieldset. */
export function GrantsEditor({ prefix, view, grants, access, onChange, platform = desktopPlatform() }: {
  prefix: string; view: PolicyView; grants: AccessGrant[]; access: AgentAccess; onChange: (grants: AccessGrant[]) => void; platform?: string;
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
    {access !== "full" && platform === "linux" && <p className="pj-note" role="note">{LINUX_PORTS_ANY_HOST}</p>}
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
