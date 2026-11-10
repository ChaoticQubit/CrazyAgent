import type { AgentAccess, AgentNetwork, AgentPolicy, PolicyView } from "../../api-projects";
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

/** What saving a new level, hooks or network choice does to the project's open IDE, the same in the TUI. */
export const POLICY_RESTARTS_IDE = "Changing the level, the IDE hooks switch below Full access, or the network choice at Own branch restarts this project's IDE at once. Its terminals end, and it reopens within a minute.";

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

/** Names a network choice as the owner sees it. Input: the choice. Output: the label. */
export function networkLabel(network: AgentNetwork): string {
  return NETWORK_CHOICES.find((choice) => choice.network === network)?.label ?? network;
}

/** The policy Save sends: the level, the network choice, a choice only for each toolchain
 * whose switch differs from the project's own use, so the others keep following the project,
 * and the IDE hooks switch. Inputs: the engine's view, the chosen level and network, one
 * switch per view toolchain and the IDE hooks switch. Output: the policy. */
export function policyOf(view: PolicyView, access: AgentAccess, network: AgentNetwork, allowed: boolean[], ideGitHooks: boolean): AgentPolicy {
  assert(allowed.length === view.toolchains.length, "one switch per toolchain");
  return {
    access,
    network,
    toolchains: view.toolchains.flatMap((row, index) => row.used === allowed[index] ? [] : [{ toolchain: row.toolchain, allowed: allowed[index] }]),
    ide_git_hooks: ideGitHooks,
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
