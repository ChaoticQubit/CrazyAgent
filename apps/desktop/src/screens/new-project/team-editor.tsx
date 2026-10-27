import type { HarnessEntry } from "../../api";
import { displayText } from "../conversation";
import { Switch as ToggleSwitch, type IconName } from "../../ui";
import type { PoolChoice, ResearchKind } from "../../api-projects";
import {
  ANY_HARNESS_NOTE, HARNESS_AGENTS_MAX, SUB_HARNESSES_MAX, TEAM_AGENTS_MAX, addHarnessRow, agentTotalText, harnessChoices, harnessLabel,
  poolFits, poolName, removeHarnessRow, setAgents, setMaster, setRowHarness, setTools, teamErrors, teamNotes,
} from "./wizard";
import type { TeamDraft } from "./wizard";

export const RESEARCH_LABELS: Record<ResearchKind, string> = {
  market_research: "Market research",
  competitor_research: "Competitor research",
  design_research: "Design research",
  front_end_design: "Front-end design",
  tech_stack_research: "Tech stack research",
};

export const RESEARCH_ICONS: Record<ResearchKind, IconName> = {
  market_research: "trending-up",
  competitor_research: "swords",
  design_research: "palette",
  front_end_design: "layout-template",
  tech_stack_research: "layers",
};

const KNOWN: Record<string, string> = { codex: "Codex", claude: "Claude Code" };

/** Names a harness from the catalog, or from the two built-in names while the catalog is not ready.
 * Inputs: the catalog or null and the harness id. Output: a display name. */
export function harnessName(harnesses: HarnessEntry[] | null, id: string): string {
  return harnesses ? harnessLabel(harnesses, id) : KNOWN[id] ?? id;
}

function HarnessSelect({ id, value, harnesses, onChange, label }: {
  id: string; value: string; harnesses: HarnessEntry[] | null; onChange: (value: string) => void; label: string;
}) {
  const choices = harnesses ? harnessChoices(harnesses) : Object.entries(KNOWN).map(([key, name]) => ({ id: key, label: name, group: "", disabled: false }));
  const groups = [...new Set(choices.map((choice) => choice.group))];
  const known = choices.some((choice) => choice.id === value);
  return <select className="pj-select" id={id} aria-label={label} value={value} onChange={(event) => onChange(event.target.value)}>
    {!known && <option value={value}>{harnessName(harnesses, value) || "Pick a harness"}</option>}
    {groups.map((group) => group
      ? <optgroup key={group} label={group}>{choices.filter((choice) => choice.group === group).map((choice) => <option key={choice.id} value={choice.id} disabled={choice.disabled}>{choice.label}</option>)}</optgroup>
      : choices.map((choice) => <option key={choice.id} value={choice.id}>{choice.label}</option>))}
  </select>;
}

function Switch({ id, checked, fixed, label, onChange }: { id: string; checked: boolean; fixed: boolean; label: string; onChange: (value: boolean) => void }) {
  return <ToggleSwitch id={id} label={label} hideLabel checked={fixed || checked} onChecked={onChange} disabledReason={fixed ? "Always on" : undefined} />;
}

/** Edits a team: master harness, agent rows with counts and tools, and the agent range.
 * Inputs: the team, the harness catalog (null while not ready), the chosen pool, research
 * kinds and a change handler. Output: the editor; it never saves on its own. */
export function TeamEditor({ team, harnesses, pool, research, onChange, prefix }: {
  team: TeamDraft;
  harnesses: HarnessEntry[] | null;
  pool: PoolChoice | null;
  research: ResearchKind[];
  onChange: (team: TeamDraft) => void;
  prefix: string;
}) {
  const errors = teamErrors(team, harnesses);
  const notes = teamNotes(team, harnesses);
  const master = harnesses?.find((harness) => harness.id === team.master);
  const fixed = (id: string) => harnesses?.find((harness) => harness.id === id)?.tools.kind === "limited";
  const researchHarnesses = [...new Set((team.sub_harnesses.length ? team.sub_harnesses.map((slot) => slot.harness) : [team.master]).map((id) => harnessName(harnesses, id)))];
  return <div className="pj-stack">
    <section className="pj-stack-s" aria-labelledby={`${prefix}-master`}>
      <h3 id={`${prefix}-master`}>Master</h3>
      <div className="pj-team-master">
        <div className="pj-field">
          <label htmlFor={`${prefix}-master-harness`}>Harness</label>
          <HarnessSelect id={`${prefix}-master-harness`} label="Master harness" value={team.master} harnesses={harnesses}
            onChange={(value) => onChange(setMaster(team, value, harnesses))} />
        </div>
        <div className="pj-field">
          <span className="pj-label">Tools</span>
          <Switch id={`${prefix}-master-tools`} checked={team.master_tools} fixed={fixed(team.master)} label="Tools for the master"
            onChange={(value) => onChange(setTools(team, "master", value, harnesses))} />
        </div>
      </div>
      {errors.master && <p className="pj-error" role="alert">{errors.master}</p>}
      {pool && master && !poolFits(master, pool.members) &&
        <p className="pj-note warn">{displayText(poolName(pool.pool))} has no account {master.name} can use. Add an account it can use to the pool, or pick another harness.</p>}
      <p className="pj-help">{pool ? `Uses an account from ${displayText(poolName(pool.pool))}.` : "Uses an account from the pool you choose."}</p>
    </section>
    <section className="pj-stack-s" aria-labelledby={`${prefix}-agents`}>
      <h3 id={`${prefix}-agents`}>Agents</h3>
      {team.sub_harnesses.length > 0 && <div className="pj-team-rows" role="table" aria-label="Agent harnesses">
        <div className="pj-team-row head" role="row"><span role="columnheader">Harness</span><span role="columnheader">Agents</span><span role="columnheader">Tools</span><span role="columnheader"><span className="pj-sr">Remove</span></span></div>
        {team.sub_harnesses.map((slot, index) => {
          const name = harnessName(harnesses, slot.harness);
          return <div className="pj-team-row" role="row" key={index}>
            <span role="cell"><HarnessSelect id={`${prefix}-row-${index}`} label={`Harness for agent row ${index + 1}`} value={slot.harness} harnesses={harnesses}
              onChange={(value) => onChange(setRowHarness(team, index, value, harnesses))} /></span>
            <span role="cell" className="pj-stepper" aria-label={`Agents for ${name}`}>
              <button type="button" aria-label={`Fewer ${name} agents`} disabled={slot.agents <= 1} onClick={() => onChange(setAgents(team, index, slot.agents - 1))}>-</button>
              <output aria-live="polite">{slot.agents}</output>
              <button type="button" aria-label={`More ${name} agents`} disabled={slot.agents >= HARNESS_AGENTS_MAX} onClick={() => onChange(setAgents(team, index, slot.agents + 1))}>+</button>
            </span>
            <span role="cell"><Switch id={`${prefix}-tools-${index}`} checked={slot.tools} fixed={fixed(slot.harness)} label={`Tools for ${name}`} onChange={(value) => onChange(setTools(team, index, value, harnesses))} /></span>
            <span role="cell"><button type="button" className="btn quiet danger-text" onClick={() => onChange(removeHarnessRow(team, index))}>Remove</button></span>
            {errors.rows[index] && <p className="pj-error" role="alert">{errors.rows[index]}</p>}
          </div>;
        })}
      </div>}
      {team.sub_harnesses.length === 0 && <p className="pj-help">{ANY_HARNESS_NOTE}</p>}
      <div><button type="button" className="btn" disabled={team.sub_harnesses.length >= SUB_HARNESSES_MAX} onClick={() => onChange(addHarnessRow(team, harnesses))}>+ Add harness</button></div>
    </section>
    <section className="pj-stack-s" aria-labelledby={`${prefix}-range`}>
      <h3 id={`${prefix}-range`}>How many at once</h3>
      <div className="pj-row">
        <label htmlFor={`${prefix}-min`}>Run at least</label>
        <input className="pj-input num" id={`${prefix}-min`} type="number" min={1} max={TEAM_AGENTS_MAX} value={team.min_agents}
          onChange={(event) => onChange({ ...team, min_agents: Number(event.target.value) })} aria-invalid={Boolean(errors.range) || undefined} />
        <label htmlFor={`${prefix}-max`}>and at most</label>
        <input className="pj-input num" id={`${prefix}-max`} type="number" min={1} max={TEAM_AGENTS_MAX} value={team.max_agents}
          onChange={(event) => onChange({ ...team, max_agents: Number(event.target.value) })} aria-invalid={Boolean(errors.range) || undefined}
          aria-describedby={errors.range ? `${prefix}-range-error` : undefined} />
        <span>agents, counting the master.</span>
      </div>
      {errors.range && <p className="pj-error" id={`${prefix}-range-error`} role="alert">{errors.range}</p>}
      <p className="pj-help">{agentTotalText(team)}</p>
    </section>
    {research.length > 0 && <section className="pj-stack-s"><h3>Research</h3>
      <p>Research uses {researchHarnesses.join(" and ")}: {research.map((kind) => RESEARCH_LABELS[kind]).join(", ")}.</p></section>}
    {notes.length > 0 && <section className="pj-stack-s" aria-label="Limited control">{notes.map((note) => <p className="pj-note warn" key={note}>{note}</p>)}</section>}
    <p className="pj-note">Every agent works in its own copy of the project (a git worktree) inside the project folder and can only write there. Tools let agents run commands and edit files, inside the project folder only. Tools on also allows network access.</p>
  </div>;
}
