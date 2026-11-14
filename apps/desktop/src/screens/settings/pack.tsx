import { useEffect, useState } from "react";
import { listSkills } from "../../api";
import { fixToolPermissions, installDependency, intakeCopy, loadSetupStatus, removePackEntry, runSetup, setPackEntry, setRefusedTool } from "../../api-intake";
import type { SkillEntry } from "../../api-process";
import type { Dependency, PackEntry, RefusedTool, SetupStatus, ToolFix } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, Icon, LoadError, NotReady, RadioCard, SettingSection, Skeleton, StatusBadge, Switch } from "../../ui";
import { SettingLine, SettingRow } from "../../ui/setting";
import { displayText } from "../conversation";
import { REFUSED_TOOL_CHOICES, SETUP_POLL_MS, dependencyView, entryIcon, entryName, fixCommand, fixFailed, installed, packGroups, packStateView, rowFix, rowInstall, setupBusy, toolsChecking, toolsOffer, toolsStep } from "../intake/intake";
import { ToolFixDialog, type ToolFixAsk } from "../intake/setup-step";
import { SkillTag, skillEntries } from "../skill-tag";
import "../project/projects.css";

export type PackAsk = { kind: "remove" | "install"; entry: PackEntry };

export const TOOLS_BUSY = "tools:";

/** Words for the confirm before a pack change: removing deletes what the entry added, and
 * installing an MCP server lets agents run it. Input: the pending change. Output: title, body
 * and confirm label. */
export function packConfirm(ask: PackAsk): { title: string; body: string; confirm: string } {
  const name = entryName(ask.entry);
  if (ask.kind === "remove") {
    return { title: `Remove ${name}?`, confirm: "Remove",
      body: `CrazyAgent deletes the ${ask.entry.kind === "mcp_server" ? "MCP server" : "skill"} it installed for every project. You can install it again from this page.` };
  }
  return { title: `Install ${name}?`, confirm: "Install",
    body: ask.entry.kind === "mcp_server" ? `Installing it lets your agents run this MCP server in every project. It comes from ${displayText(ask.entry.source)}.`
      : `Agents in every project can use this skill. It comes from ${displayText(ask.entry.source)}.` };
}

/** The Tools section of Settings, Pack: tmux, uv, graphify and Node with their state, any
 * readiness note such as agent git needing the Command Line Tools, Install tools while one is
 * missing, failed or not used and nothing is checking or installing, Install on a row that
 * installs alone, and on a tool found and not used its fix or the command by the setting.
 * Inputs: the setup status, the change in flight and the Install tools, row Install and Fix
 * permissions handlers. */
function ToolsSection({ status, busy, onInstallTools, onInstall, onFix }: {
  status: SetupStatus;
  busy: string | null;
  onInstallTools: () => void;
  onInstall?: (dependency: Dependency) => void;
  onFix?: (dependency: Dependency, fix: ToolFix) => void;
}) {
  const step = toolsStep({ kind: "ok", value: status });
  const offer = toolsOffer(status);
  return <SettingSection title="Tools" about={step.body} actions={offer ? <Button variant="primary" size="small" icon="wrench" disabled={busy !== null && busy !== TOOLS_BUSY} pending={busy === TOOLS_BUSY}
    pendingLabel="Starting..." onClick={onInstallTools}>Install tools</Button> : undefined}>
    {status.dependencies.map((dependency) => {
      const row = dependencyView(dependency);
      const fix = onFix ? rowFix(dependency, status) : null;
      const command = fixCommand(dependency, status);
      const failed = fixFailed(dependency);
      return <SettingRow key={dependency.name} label={row.title}>
        <span className="set-label">{row.title}</span>
        <span className="set-help">
          <span>{row.about}</span>
          {row.line && <span className={dependency.state.state === "failed" ? "pj-error" : undefined}>{row.line}</span>}
          {failed && <span className="pj-error">{failed}</span>}
          {command && <span>To fix it, run in Terminal: <code>{command}</code></span>}
        </span>
        <div className="set-ctl set-line-ctl"><StatusBadge word={row.word} tone={row.tone} />
          {fix && onFix && <Button size="small" icon="wrench" disabled={busy !== null} onClick={() => onFix(dependency, fix)}>Fix permissions</Button>}
          {onInstall && rowInstall(dependency, status) && <Button size="small" icon="wrench" disabled={busy !== null} pending={busy === TOOLS_BUSY + dependency.name}
            pendingLabel="Starting..." onClick={() => onInstall(dependency)}>Install</Button>}</div>
      </SettingRow>;
    })}
    {status.notes && status.notes.length > 0 && <div className="set-body">{status.notes.map((note) => <p className="pj-error" key={note}>{displayText(note)}</p>)}</div>}
  </SettingSection>;
}

/** The Unused tools setting of Settings, Pack (DEC-162): whether a tool found and not used
 * because others can change its files offers a fix button or shows only the command, the
 * fix button when the engine names neither. Inputs: the setup status, the change in flight
 * and the handler. */
function RefusedToolSection({ status, busy, onChoice }: { status: SetupStatus; busy: string | null; onChoice: (choice: RefusedTool) => void }) {
  const choice = status.refused_tool ?? "fix_button";
  return <SettingSection title="Unused tools">
    <SettingLine wide label="When setup finds a tool it does not use" help="Others can change files the tool runs, so CrazyAgent passes it over.">
      <fieldset className="field" disabled={busy !== null}>
        <legend className="visually-hidden">When setup finds a tool it does not use</legend>
        {REFUSED_TOOL_CHOICES.map((option) => <RadioCard key={option.value} name="unused-tools" value={option.value} checked={choice === option.value}
          onChecked={() => onChoice(option.value)} title={option.title} details={[option.detail]} />)}
      </fieldset>
    </SettingLine>
  </SettingSection>;
}

/** Tells whether a pack group differs from what ships: an installed entry that is on when it
 * ships off or off when it ships on. Entries not installed have no state to compare.
 * Input: the group's entries. Output: true when any differs. */
function differsFromShipped(entries: PackEntry[]): boolean {
  return entries.some((entry) => installed(entry) && (entry.state.state === "on") !== entry.on_by_default);
}

/** One pack entry: its name and state, where it comes from and any detail on the left, and on
 * the right a switch and Remove when installed, or Install or Try again when not.
 * Inputs: the entry, whether changes wait, the change in flight, the skills and the handlers. */
function PackRow({ entry, locked, busy, skills, onSwitch, onAsk }: {
  entry: PackEntry;
  locked: boolean;
  busy: string | null;
  skills: readonly SkillEntry[];
  onSwitch: (entry: PackEntry, on: boolean) => void;
  onAsk: (ask: PackAsk) => void;
}) {
  const state = packStateView(entry.state, entry.hooks_refused);
  return <SettingRow label={entryName(entry)}>
    <span className="set-label row"><Icon name={entryIcon(entry)} size={14} />{entry.kind === "skill" ? <SkillTag name={entry.id} label={entryName(entry)} skills={skills} /> : entryName(entry)}{entry.kind === "mcp_server" && <span className="pj-small pj-muted">MCP server</span>}
      <StatusBadge word={state.word} tone={state.tone} /></span>
    <span className="set-help">
      <span>{displayText(entry.source)}, {displayText(entry.license)}</span>
      {state.line && <span className="pj-error">{state.line}</span>}
    </span>
    <div className="set-ctl set-line-ctl">
      {installed(entry) && <Switch id={`pack-${entry.id}`} label={`Use ${entryName(entry)}`} hideLabel checked={entry.state.state === "on"}
        disabledReason={locked ? "Wait for the current change." : undefined} onChecked={(on) => onSwitch(entry, on)} />}
      {installed(entry) && <Button size="small" variant="danger-text" disabled={locked} onClick={() => onAsk({ kind: "remove", entry })}>Remove</Button>}
      {!installed(entry) && entry.state.state !== "installing" && <Button size="small" disabled={locked} pending={busy === entry.id}
        pendingLabel="Starting..." onClick={() => onAsk({ kind: "install", entry })}>{entry.state.state === "failed" ? "Try again" : "Install"}</Button>}
    </div>
  </SettingRow>;
}

/** Settings, Pack as a view of its state: the title in every state, then any running or failure
 * line, the default pack grouped like setup and last tmux, uv and graphify. A group whose
 * installed entries differ from what ships is marked changed. Inputs: the setup read, the
 * change in flight, the last problem and the handlers. */
export function PackSettingsView({ status, busy, problem, skills, onSwitch, onAsk, onInstallTools, onInstall, onFix, onRefusedTool, onRetry }: {
  status: Loaded<SetupStatus>;
  busy: string | null;
  problem: string;
  skills: readonly SkillEntry[];
  onSwitch: (entry: PackEntry, on: boolean) => void;
  onAsk: (ask: PackAsk) => void;
  onInstallTools: () => void;
  onInstall?: (dependency: Dependency) => void;
  onFix?: (dependency: Dependency, fix: ToolFix) => void;
  onRefusedTool?: (choice: RefusedTool) => void;
  onRetry: () => void;
}) {
  const running = status.kind === "ok" && setupBusy(status.value);
  return <div className="stack">
    <div className="settings-top"><div><h2>Pack</h2><p>The skills and plugins every project starts with. Switching one off stops agents using it; removing it deletes what it installed.</p></div></div>
    {status.kind === "loading" ? <Skeleton shape="row" count={6} />
      : status.kind === "not_ready" ? <NotReady what="The default pack" />
      : status.kind === "error" ? <LoadError message={status.message} onRetry={onRetry} />
      : <>
        {running && <p className="pj-secondary" role="status">Setup is installing. Changes wait until it finishes.</p>}
        {problem && <p className="pj-error" role="alert">{problem}</p>}
        {packGroups(status.value.pack).map((group) => <SettingSection key={group.title} about={group.about || undefined}
          title={differsFromShipped(group.entries) ? <>{group.title}<span className="set-changed">changed</span></> : group.title}>
          {group.entries.map((entry) => <PackRow key={entry.id} entry={entry} locked={running || busy !== null} busy={busy} skills={skills} onSwitch={onSwitch} onAsk={onAsk} />)}
        </SettingSection>)}
        {status.value.dependencies.length > 0 && <ToolsSection status={status.value} busy={busy} onInstallTools={onInstallTools} onInstall={onInstall} onFix={onFix} />}
        {status.value.dependencies.length > 0 && onRefusedTool && <RefusedToolSection status={status.value} busy={busy} onChoice={onRefusedTool} />}
      </>}
  </div>;
}

/** The confirm before a pack change: Remove in a danger confirm, Install in a plain one, and
 * neither closes or cancels while a change is in flight, so a request that still lands is never
 * shown as cancelled. Inputs: the pending change, whether a change is in flight and the handlers. */
export function PackConfirmDialogs({ ask, pending, onCancel, onConfirm }: {
  ask: PackAsk | null;
  pending: boolean;
  onCancel: () => void;
  onConfirm: (ask: PackAsk) => void;
}) {
  const words = ask ? packConfirm(ask) : null;
  return <>
    <ConfirmDialog open={ask?.kind === "remove"} title={words?.title ?? ""} confirmLabel={words?.confirm ?? "Remove"} pending={pending}
      onCancel={onCancel} onConfirm={() => { if (ask) onConfirm(ask); }}>
      {words?.body}
    </ConfirmDialog>
    <Dialog open={ask?.kind === "install"} title={words?.title ?? ""} busy={pending} onClose={onCancel}
      footer={<><Button disabled={pending} onClick={onCancel}>Cancel</Button>
        <Button variant="primary" pending={pending} pendingLabel="Starting..." onClick={() => { if (ask) onConfirm(ask); }}>{words?.confirm ?? "Install"}</Button></>}>
      <p>{words?.body}</p>
    </Dialog>
  </>;
}

/** Settings, Pack: reads the tools and the pack, installs missing tools, fixes a refused tool's
 * permissions once the owner confirms the tree, saves what unused tools offer, switches, removes
 * and installs entries, and reads again every few seconds while a tool is checked or setup
 * installs. */
export function PackSettings() {
  const session = useAppSession();
  const [loaded, reload] = useLoad(loadSetupStatus, [], "setup-status");
  const [listed] = useLoad(() => listSkills(), [undefined], "skills");
  const [fresh, setFresh] = useState<SetupStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [ask, setAsk] = useState<PackAsk | null>(null);
  const [fixAsk, setFixAsk] = useState<ToolFixAsk | null>(null);
  const [problem, setProblem] = useState("");
  const status: Loaded<SetupStatus> = fresh ? { kind: "ok", value: fresh } : loaded;
  const moving = status.kind === "ok" && (setupBusy(status.value) || toolsChecking(status.value));
  const version = status.kind === "ok" ? status.value.version : 0;

  useEffect(() => { if (loaded.kind === "ok") setFresh(loaded.value); else setFresh(null); }, [loaded]);
  useEffect(() => {
    if (!moving) return;
    const timer = window.setTimeout(reload, SETUP_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [moving, status]);

  const apply = async (key: string, change: () => ReturnType<typeof setPackEntry>, done: string) => {
    if (busy) return;
    setBusy(key);
    setProblem("");
    const result = await change();
    setBusy(null);
    setAsk(null);
    setFixAsk(null);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Changing the pack isn't in this build." : intakeCopy(result)); reload(); return; }
    setFresh(result.value);
    session.showToast(done);
  };

  const confirm = (pending: PackAsk) => {
    const name = entryName(pending.entry);
    if (pending.kind === "remove") void apply(pending.entry.id, () => removePackEntry(pending.entry.id, version), `${name} was removed.`);
    else void apply(pending.entry.id, () => runSetup([pending.entry.id]), `Installing ${name}.`);
  };

  return <>
    <PackSettingsView status={status} busy={busy} problem={problem} skills={listed.kind === "ok" ? skillEntries(listed.value) : []} onRetry={reload} onAsk={setAsk}
      onInstallTools={() => void apply(TOOLS_BUSY, () => runSetup([]), "Installing the missing tools.")}
      onInstall={(dependency) => void apply(TOOLS_BUSY + dependency.name, () => installDependency(dependency.name), `Installing ${dependency.label}.`)}
      onFix={(dependency, fix) => setFixAsk({ dependency, fix })}
      onRefusedTool={(choice) => void apply(TOOLS_BUSY, () => setRefusedTool(choice), `Unused tools: ${REFUSED_TOOL_CHOICES.find((option) => option.value === choice)?.title ?? choice}.`)}
      onSwitch={(entry, on) => void apply(entry.id, () => setPackEntry(entry.id, on, version), `${entryName(entry)} is ${on ? "on" : "off"}.`)} />
    <ToolFixDialog ask={fixAsk} pending={busy !== null} onCancel={() => { if (busy === null) setFixAsk(null); }}
      onConfirm={(pending) => void apply(TOOLS_BUSY + pending.dependency.name, () => fixToolPermissions(pending.dependency.name, pending.fix.tree, pending.fix.recursive), `Fixing permissions on ${displayText(pending.fix.tree)}.`)} />
    <PackConfirmDialogs ask={ask} pending={busy !== null} onCancel={() => { if (busy === null) setAsk(null); }} onConfirm={confirm} />
  </>;
}
