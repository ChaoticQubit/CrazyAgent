import { useEffect, useState } from "react";
import { listMcpServers, type HarnessEntry, type McpServerView } from "../../api";
import { failureCopy, loadAgentPolicy, type AgentAccess, type PolicyView, type PoolChoice, type StoredSetup } from "../../api-projects";
import { useHeld, useLoad } from "../../state";
import { Button, ConfirmDialog, Icon, Segmented, Select, Switch, type SelectOption } from "../../ui";
import { displayText } from "../conversation";
import { ACCESS_LEVELS, FULL_ACCESS_WARNING, LOWERING_STOPS_AGENTS, accessLabel, confirmFirst, policyOf } from "../project/access";
import { NO_POOL, accessSaved, saveAccess, savePool, saveTeam } from "../project/settings";
import { valueOf, type ProjectData } from "../project/shared";
import { harnessName } from "../new-project/team-editor";
import { HARNESS_AGENTS_MAX, anyHarnessNote, entry, harnessChoices, poolName, setAgents, setMaster, setTools, teamErrors, teamFull, teamValid } from "../new-project/wizard";
import type { TeamDraft } from "../new-project/wizard";
import { SettingRow } from "./controls";

/** The parts of a project's data the in-place team, pool and access rows read and save with. */
export type TeamProject = Pick<ProjectData, "id" | "setup" | "harnesses" | "pools" | "assigned" | "notify" | "reload" | "reloadPools">;

/** The MCP servers a project's agents can reach: each one on, on in this project and approved,
 * in the engine's order. Input: the project's servers read. Output: their names. */
export function reachableServers(servers: McpServerView[]): string[] {
  return servers.filter((server) => server.enabled && server.on_here !== false && server.status === "approved").map((server) => server.name);
}

/** The Account choices of the master's settings: any account of each pool, since the master
 * fails over across a pool's accounts and is never pinned to one, then no pool. Input: the
 * pools. Output: options whose value is the pool id, or "" for no pool. */
export function accountOptions(pools: PoolChoice[]): SelectOption[] {
  return [...pools.map((choice) => ({ value: choice.pool.id, label: `Any in ${displayText(poolName(choice.pool))}` })), { value: "", label: NO_POOL.label }];
}

function teamProblem(team: TeamDraft, harnesses: HarnessEntry[] | null): string {
  const errors = teamErrors(team, harnesses);
  return errors.master ?? errors.range ?? errors.rows.find((row) => row !== undefined) ?? "Check the team in project Settings, Team.";
}

function masterOptions(harnesses: HarnessEntry[] | null, master: string): { value: string; label: string; disabled: boolean }[] {
  const choices = harnesses ? harnessChoices(harnesses, "master").map((choice) => ({ value: choice.id, label: choice.label, disabled: choice.disabled })) : [];
  const shown = entry(harnesses, master)?.id ?? master;
  return choices.some((choice) => choice.value === shown) ? choices : [{ value: shown, label: harnessName(harnesses, shown) || "Pick a harness", disabled: false }, ...choices];
}

/** The saved team a row shows and the call that saves a change to it through project Settings'
 * own team save: the change is checked as Settings checks it, and the row keeps showing it until
 * the setup is read again at a newer revision. Input: the project data. Output: the stored setup
 * or null, the team shown, whether a save runs, its note and the save. */
function useTeam(data: TeamProject | undefined) {
  const setup: StoredSetup | null = valueOf(data?.setup);
  const [held, setHeld] = useState<{ team: TeamDraft; revision: number | null } | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const team: TeamDraft | null = setup ? (held !== null && held.revision === setup.revision ? held.team : setup.team) : null;

  async function save(next: TeamDraft) {
    if (!data || !setup || busy) return;
    const harnesses = valueOf(data.harnesses);
    if (!teamValid(next, harnesses)) return setNote(teamProblem(next, harnesses));
    setHeld({ team: next, revision: setup.revision });
    setBusy(true);
    const message = await saveTeam(data, setup, next);
    setBusy(false);
    if (message !== "") setHeld(null);
    setNote(message);
  }

  return { setup, team, busy, note, save };
}

function TeamState({ data }: { data: TeamProject | undefined }) {
  if (data?.setup === undefined) return <span className="tg-terminal-note" role="status">Loading the team...</span>;
  if (data.setup.kind === "not_ready") return <span className="tg-terminal-note">Not ready yet: the project's team isn't in this build.</span>;
  if (data.setup.kind === "error") return <span className="tg-terminal-note" role="alert">Couldn't load the team. {failureCopy(data.setup)}</span>;
  return <span className="tg-terminal-note">This project has no saved team yet.</span>;
}

function RowNote({ note }: { note: string }) {
  return note ? <p className="pj-note warn" role="alert">{displayText(note)}</p> : null;
}

/** The master's harness as a select of the harness catalog, saved in place through project
 * Settings' team save, on Agent settings and in the master's settings. Inputs: the project
 * data, the row's label, the select's id and an optional help line. Output: the row. */
export function MasterHarnessSetting({ data, label, id, helper }: { data: TeamProject | undefined; label: string; id: string; helper?: string }) {
  const { team, busy, note, save } = useTeam(data);
  const harnesses = valueOf(data?.harnesses);
  if (team === null) return <SettingRow label={label}><span className="set-label">{label}</span><TeamState data={data} /></SettingRow>;
  const shown = entry(harnesses, team.master)?.id ?? team.master;
  return <SettingRow label={label}>
    <div className="field">
      <label htmlFor={id}>{label}</label>
      <select id={id} className="select" value={shown} disabled={busy} aria-describedby={helper ? `${id}-help` : undefined}
        onChange={(event) => void save(setMaster(team, event.target.value, harnesses))}>
        {masterOptions(harnesses, team.master).map((option) => <option key={option.value} value={option.value} disabled={option.disabled}>{option.label}</option>)}
      </select>
      {helper && <span className="helper" id={`${id}-help`}>{helper}</span>}
    </div>
    <RowNote note={note} />
  </SettingRow>;
}

/** Each sub-agent harness of the saved team with how many agents it may run at once and its
 * Tools switch, changed in place and saved through project Settings' team save. Inputs: the
 * project data and the id prefix of its switches. Output: the row. */
export function SubHarnessesSetting({ data, prefix }: { data: TeamProject | undefined; prefix: string }) {
  const { team, busy, note, save } = useTeam(data);
  const harnesses = valueOf(data?.harnesses);
  const label = "Sub-agent harnesses";
  if (team === null) return <SettingRow label={label} wide><span className="set-label">{label}</span><TeamState data={data} /></SettingRow>;
  const full = teamFull(team);
  return <SettingRow label={label} wide>
    <span className="set-label">{label}</span>
    {team.sub_harnesses.length > 0 ? <div className="set-slots" role="table" aria-label={label}>
      <div className="set-slot head" role="row"><span role="columnheader">Harness</span><span role="columnheader">Agents</span><span role="columnheader">Tools</span></div>
      {team.sub_harnesses.map((slot, index) => {
        const name = harnessName(harnesses, slot.harness);
        const fixed = entry(harnesses, slot.harness)?.tools.kind === "limited";
        return <div className="set-slot" role="row" key={index}>
          <span role="cell">{displayText(name)}</span>
          <span role="cell"><span className="pj-stepper" role="group" aria-label={`Agents for ${name}`}>
            <button type="button" aria-label={`Fewer ${name} agents`} disabled={busy || slot.agents <= 1} onClick={() => void save(setAgents(team, index, slot.agents - 1))}>−</button>
            <output aria-live="polite">{slot.agents}</output>
            <button type="button" aria-label={`More ${name} agents`} disabled={busy || slot.agents >= HARNESS_AGENTS_MAX || full} onClick={() => void save(setAgents(team, index, slot.agents + 1))}>+</button>
          </span></span>
          <span role="cell"><Switch id={`${prefix}-tools-${index}`} label={`Tools for ${name}`} hideLabel checked={fixed || slot.tools}
            onChecked={(on) => void save(setTools(team, index, on, harnesses))} disabledReason={fixed ? "Always on" : busy ? "Saving the last change." : undefined} /></span>
        </div>;
      })}
    </div> : <span className="tg-terminal-note">{anyHarnessNote(team)}</span>}
    <span className="tg-terminal-note">How many agents each harness may run at once. Add or remove a harness in project Settings, Team.</span>
    <RowNote note={note} />
  </SettingRow>;
}

/** Says what saving a moved access level does, before Save: a lowering stops the agents
 * running above the new level. Inputs: the saved level and the chosen one, which differ.
 * Output: the line. */
export function accessChangeLine(from: AgentAccess, to: AgentAccess): string {
  if (from === to) throw new Error("an access change line names two different levels");
  const rank = (access: AgentAccess) => ACCESS_LEVELS.findIndex((level) => level.access === access);
  return rank(to) < rank(from)
    ? `Saving lowers agent access from ${accessLabel(from)} to ${accessLabel(to)}: agents running above ${accessLabel(to)} stop and restart at it.`
    : `Saving raises agent access from ${accessLabel(from)} to ${accessLabel(to)}.`;
}

/** The project's agent access as a choice of its levels: a click or an arrow key only moves
 * the choice, and Save saves it through project Settings' own access save, Full access after
 * the same confirmation; the network and toolchain choices stay as saved. Inputs: the project
 * id, how to tell the owner and a team reload. Output: the row. */
export function AgentAccessSetting({ projectId, notify, reload }: { projectId: string; notify?: (message: string) => void; reload: () => void }) {
  const [view, setView] = useState<PolicyView | null>(null);
  const [failure, setFailure] = useState("");
  const [chosen, setChosen] = useState<AgentAccess | null>(null);
  const [asked, setAsked] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const label = "Agent access";
  useEffect(() => {
    let live = true;
    void loadAgentPolicy(projectId).then((result) => {
      if (!live) return;
      if (result.kind === "ok") setView(result.value);
      else setFailure(result.kind === "not_ready" ? "Not ready yet: agent access isn't in this build." : failureCopy(result));
    });
    return () => { live = false; };
  }, [projectId]);

  async function save(access: AgentAccess) {
    if (!view) return;
    setAsked(false);
    setBusy(true);
    const saved = await saveAccess(projectId, view, policyOf(view, access, view.network, view.toolchains.map((row) => row.allowed), view.ide_git_hooks));
    setBusy(false);
    if (saved.fresh) setView(saved.fresh);
    else if (saved.note === "") setView({ ...view, access });
    if (saved.fresh || saved.note === "") setChosen(null);
    setNote(saved.note);
    if (saved.note === "") {
      notify?.(accessSaved(access));
      reload();
    }
  }

  function pick(access: AgentAccess) {
    if (!view || busy) return;
    const reason = view.levels.find((level) => level.access === access)?.unavailable ?? null;
    if (reason !== null) return setNote(`${accessLabel(access)} can't run here: ${reason}`);
    setNote("");
    setChosen(access === view.access ? null : access);
  }

  function submit() {
    if (!view || busy || chosen === null) return;
    if (confirmFirst(chosen)) setAsked(true);
    else void save(chosen);
  }

  return <SettingRow label={label}>
    <span className="set-label" aria-hidden={view ? true : undefined}>{label}</span>
    {view
      ? <Segmented label={label} value={chosen ?? view.access} onValue={pick} options={ACCESS_LEVELS.map((level) => ({ value: level.access, label: level.label }))} />
      : <span className="set-ctl tg-terminal-note" role={failure ? "alert" : "status"}>{failure || "Loading..."}</span>}
    <span className="tg-terminal-note">Sandboxed keeps agents in their worktree with today's limits. Own branch gives each agent full git on its own branch. Full access lets them run as you. {LOWERING_STOPS_AGENTS}</span>
    {view && chosen !== null && <div className="pj-row">
      <span className="tg-terminal-note" role="status">{accessChangeLine(view.access, chosen)}</span>
      <Button variant="primary" size="small" pending={busy} pendingLabel="Saving..." onClick={submit}>Save agent access</Button>
      <Button size="small" disabled={busy} onClick={() => setChosen(null)}>{`Keep ${accessLabel(view.access)}`}</Button>
    </div>}
    <ConfirmDialog open={asked} title="Give this project's agents Full access?" confirmLabel="Allow Full access" cancelLabel="Keep current access" pending={busy}
      onCancel={() => setAsked(false)} onConfirm={() => { if (chosen !== null) void save(chosen); }}>
      {FULL_ACCESS_WARNING}
    </ConfirmDialog>
    <RowNote note={note} />
  </SettingRow>;
}

/** The MCP servers this project's agents can reach, as chips from the project's servers read.
 * Input: the project id. Output: the row. */
export function McpServersSetting({ projectId }: { projectId: string }) {
  const [servers] = useLoad(() => listMcpServers(projectId), [projectId], "mcp-servers");
  const names = servers.kind === "ok" ? reachableServers(servers.value) : [];
  return <SettingRow label="MCP servers">
    <span className="set-label">MCP servers</span>
    <span className="set-ctl">{servers.kind === "loading" ? <span className="tg-terminal-note" role="status">Loading...</span>
      : servers.kind !== "ok" ? <span className="tg-terminal-note">Couldn't read the servers.</span>
      : names.length === 0 ? <span className="tg-terminal-note">None</span>
      : <span className="chips">{names.map((name) => <span className="chip" key={name}><Icon name="plug" size={12} />{displayText(name)}</span>)}</span>}</span>
    <span className="tg-terminal-note">Servers this project's agents can reach. Add or approve one in project Settings, Plugins.</span>
  </SettingRow>;
}

/** The master's account as a select of the project's pools, any account of the chosen pool,
 * saved in place through project Settings' own pool save. Inputs: the project data and the
 * select's id. Output: the row. */
export function AccountSetting({ data, id }: { data: TeamProject; id: string }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const listed = valueOf(data.pools) ?? [];
  const pools = data.assigned && !listed.some((choice) => choice.pool.id === data.assigned?.pool.id) ? [data.assigned, ...listed] : listed;
  const current = data.assigned?.pool.id ?? "";
  const [shown, hold] = useHeld(current);

  async function choose(value: string) {
    if (busy || value === current) return;
    const settled = hold(value);
    setBusy(true);
    const error = await savePool(data, valueOf(data.setup), value === "" ? null : value);
    setBusy(false);
    settled(error);
    setNote(error);
  }

  return <SettingRow label="Account">
    <Select id={id} label="Account" value={shown} options={accountOptions(pools)} onValue={(value) => void choose(value)}
      helper="From the project's pool; fails over to the next account." />
    <RowNote note={note} />
  </SettingRow>;
}
