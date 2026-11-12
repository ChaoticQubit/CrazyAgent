import type { AgentAccess, AgentNetwork, AgentPolicy, IdeAgentFiles, PolicyView, SharedAccess, SharedFolders, SharedScope, ToolCacheScope } from "../../api-projects";
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
  { access: "read_write", label: "Read-write", help: "Agents may also change files in shared top-level folders git does not track. Tracked files change only in their worktree." },
];

/** What stays closed in shared folders, the same in the TUI. */
export const SHARED_SECRETS_LINE = "Secret files stay closed to agents wherever they sit: .env files, keys, certificates and credential files. Their names stay listed.";

/** How the owner shares a secret back for reading. */
export const SHARED_SECRETS_HOW = "To let agents read one, add its path inside the project folder, one per line.";

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
 * the IDE hooks switch, the agent folders choice, the tool cache choice and the shared
 * folders choice. Inputs: the engine's view, the chosen level and network, one switch per
 * view toolchain, the IDE hooks switch, the agent folders choice, and the tool cache and
 * shared folders choices, the saved ones when left out. Output: the policy. */
export function policyOf(view: PolicyView, access: AgentAccess, network: AgentNetwork, allowed: boolean[], ideGitHooks: boolean, ideAgentFiles: IdeAgentFiles, toolCache: ToolCacheScope = view.tool_cache, sharedFolders: SharedFolders = view.shared_folders ?? DEFAULT_SHARED_FOLDERS): AgentPolicy {
  assert(allowed.length === view.toolchains.length, "one switch per toolchain");
  assert(sharedFolders.chosen.length <= SHARED_CHOSEN_MAX && sharedFolders.secrets.length <= SHARED_SECRETS_MAX, "shared folders within the engine's bounds");
  return {
    access,
    network,
    toolchains: view.toolchains.flatMap((row, index) => row.used === allowed[index] ? [] : [{ toolchain: row.toolchain, allowed: allowed[index] }]),
    ide_git_hooks: ideGitHooks,
    tool_cache: toolCache,
    ide_agent_files: ideAgentFiles,
    shared_folders: sharedFolders,
  };
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

/** The shared folders choice: which local folders agents see as radio cards, the top-level
 * folders to pick for Chosen folders, Read-only or Read-write, and the secret paths shared
 * back, with what stays closed, the Linux and Windows gap, when a change applies and a note
 * when Full access makes it moot. Inputs: the id prefix, the choice, the secret paths text,
 * the engine's top-level folders, the chosen access level and the change handlers. Output:
 * the fieldset. */
export function SharedFoldersChoices({ prefix, value, secretsText, local, access, onChange, onSecretsText }: {
  prefix: string; value: SharedFolders; secretsText: string; local: string[]; access: AgentAccess;
  onChange: (value: SharedFolders) => void; onSecretsText: (text: string) => void;
}) {
  const folders = pickableFolders(local, value.chosen);
  const full = value.chosen.length >= SHARED_CHOSEN_MAX;
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
