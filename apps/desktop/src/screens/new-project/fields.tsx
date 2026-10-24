import type { Result } from "../../api";
import { useState } from "react";
import type { Platform, ProjectCatalog, ResearchKind } from "../../api-projects";
import { Icon, Switch } from "../../ui";
import { displayText } from "../conversation";
import { RESEARCH_ICONS, RESEARCH_LABELS } from "./team-editor";
import { RESEARCH_COPY, addStack, removeStack } from "./wizard";
import type { ProjectDraft, basicsErrors } from "./wizard";

const PLATFORMS: { kind: Platform; label: string }[] = [
  { kind: "website", label: "Website" },
  { kind: "mobile_app", label: "Mobile app" },
  { kind: "desktop_app", label: "Desktop app" },
  { kind: "cli", label: "CLI" },
  { kind: "api_backend", label: "API or backend" },
  { kind: "other", label: "Other" },
];

const GROUP_LABELS = { language: "Languages", framework: "Frameworks", database: "Databases" } as const;

const toggle = <T,>(list: T[], value: T) => list.includes(value) ? list.filter((item) => item !== value) : [...list, value];

/** The Basics controls shared by New Project, Project Settings, General and the setup review:
 * name, description, tech stack chips with custom entries, and target platforms with an Other text.
 * Inputs: an id prefix, the draft, its change handler, the engine catalog, the field errors, whether
 * to show the name error yet, and the fields to mark Missing. Output: the fields. */
export function BasicsFields({ prefix, draft, onChange, catalog, errors, showErrors, autoFocus = false, missing = [] }: {
  prefix: string; draft: ProjectDraft; onChange: (draft: ProjectDraft) => void; catalog: Result<ProjectCatalog> | undefined;
  errors: ReturnType<typeof basicsErrors>; showErrors: boolean; autoFocus?: boolean; missing?: string[];
}) {
  const [stackQuery, setStackQuery] = useState("");
  const [stackError, setStackError] = useState("");
  const catalogValue = catalog?.kind === "ok" ? catalog.value : null;
  const stackOptions = (catalogValue?.tech_stack ?? []).filter((option) => !stackQuery.trim() || option.label.toLowerCase().includes(stackQuery.trim().toLowerCase()));
  const platforms = catalogValue?.platforms ?? PLATFORMS;
  const mark = (field: string) => missing.includes(field) && <span className="pj-missing"> Missing</span>;
  const addCustom = () => {
    const result = addStack(draft.stack, stackQuery);
    setStackError(result.error ?? "");
    if (!result.error) { onChange({ ...draft, stack: result.stack }); setStackQuery(""); }
  };

  return <>
    <div className="pj-field">
      <label htmlFor={`${prefix}-name`}>Project name{mark("name")}</label>
      <input className="pj-input" id={`${prefix}-name`} value={draft.name} maxLength={200} autoFocus={autoFocus} aria-invalid={Boolean(showErrors && errors.name) || undefined}
        aria-describedby={showErrors && errors.name ? `${prefix}-name-error` : undefined} onChange={(event) => onChange({ ...draft, name: event.target.value })} />
      {showErrors && errors.name && <p className="pj-error" id={`${prefix}-name-error`} role="alert">{errors.name}</p>}
    </div>
    <div className="pj-field">
      <label htmlFor={`${prefix}-about`}>What is it about?{mark("about")}</label>
      <textarea className="pj-input" id={`${prefix}-about`} rows={5} value={draft.about} aria-describedby={`${prefix}-about-help`} onChange={(event) => onChange({ ...draft, about: event.target.value })} />
      <p className="pj-help" id={`${prefix}-about-help`}>Who it's for and what it should do. Agents use this for research.{draft.about.length > 3600 ? ` ${draft.about.length} of 4000.` : ""}</p>
      {errors.about && <p className="pj-error" role="alert">{errors.about}</p>}
    </div>
    <fieldset className="pj-field">
      <legend>Tech stack{mark("tech_stack")}</legend>
      {draft.stack.length > 0 && <div className="pj-chips" aria-label="Picked tech stack">{draft.stack.map((item) => <span className="pj-chip" key={item}>{displayText(item)}
        <button type="button" aria-label={`Remove ${item}`} onClick={() => onChange({ ...draft, stack: removeStack(draft.stack, item) })}>{"×"}</button></span>)}</div>}
      <div className="pj-row">
        <label className="pj-sr" htmlFor={`${prefix}-stack-search`}>Search or add a tech stack item</label>
        <input className="pj-input" id={`${prefix}-stack-search`} value={stackQuery} placeholder="Search, or type your own and press Enter" aria-describedby={`${prefix}-stack-help`} onChange={(event) => { setStackQuery(event.target.value); setStackError(""); }}
          onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addCustom(); } }} />
        <button type="button" className="btn" disabled={!stackQuery.trim()} onClick={addCustom}>Add custom</button>
      </div>
      <p className="pj-help" id={`${prefix}-stack-help`}>Pick from the list or add your own.</p>
      {stackError && <p className="pj-error" role="alert">{stackError}</p>}
      {catalogValue ? (["language", "framework", "database"] as const).map((group) => {
        const options = stackOptions.filter((option) => option.group === group);
        return options.length > 0 && <div className="pj-stack-group" key={group}><span className="pj-small pj-muted">{GROUP_LABELS[group]}</span>
          <div className="pj-chips">{options.map((option) => {
            const on = draft.stack.some((item) => item.toLowerCase() === option.label.toLowerCase());
            return <button type="button" key={option.id} className={`pj-chip-toggle ${on ? "on" : ""}`} aria-pressed={on}
              onClick={() => onChange({ ...draft, stack: on ? draft.stack.filter((item) => item.toLowerCase() !== option.label.toLowerCase()) : addStack(draft.stack, option.label).stack })}>{option.label}</button>;
          })}</div></div>;
      }) : catalog === undefined ? <p className="pj-help" role="status">Loading the tech stack list...</p>
        : <p className="pj-help">The tech stack list isn't available. Add your own entries.</p>}
    </fieldset>
    <fieldset className="pj-field">
      <legend>Target platforms{mark("platforms")}</legend>
      <div className="pj-chips">{platforms.map((platform) => <label className={`pj-chip-toggle ${draft.platforms.includes(platform.kind) ? "on" : ""}`} key={platform.kind}>
        <input type="checkbox" className="pj-sr" checked={draft.platforms.includes(platform.kind)} onChange={() => onChange({ ...draft, platforms: toggle(draft.platforms, platform.kind) })} />{platform.label}</label>)}</div>
      {draft.platforms.includes("other") && <div className="pj-field">
        <label htmlFor={`${prefix}-other`}>Other platform</label>
        <input className="pj-input" id={`${prefix}-other`} value={draft.otherPlatform} onChange={(event) => onChange({ ...draft, otherPlatform: event.target.value })} />
        {errors.otherPlatform && <p className="pj-error" role="alert">{errors.otherPlatform}</p>}
      </div>}
    </fieldset>
  </>;
}

/** The research switches shared by New Project, Work and Project Settings, General: one switch
 * per research kind with its icon and plain description. Inputs: an id prefix, the engine catalog,
 * the chosen kinds and their change handler. Output: the switch list. */
export function ResearchSwitches({ prefix, catalog, value, onChange }: {
  prefix: string; catalog: Result<ProjectCatalog> | undefined; value: ResearchKind[]; onChange: (research: ResearchKind[]) => void;
}) {
  const research = catalog?.kind === "ok" ? catalog.value.research : (Object.keys(RESEARCH_LABELS) as ResearchKind[]).map((kind) => ({ kind, label: RESEARCH_LABELS[kind] }));
  return <div className="pj-switch-list">{research.map((item) => {
    const on = value.includes(item.kind);
    return <div className="pj-switch-item" key={item.kind}>
      <Icon name={RESEARCH_ICONS[item.kind]} size={18} />
      <span><strong>{item.label}</strong><span className="pj-secondary">{RESEARCH_COPY[item.kind]}</span></span>
      <Switch id={`${prefix}-research-${item.kind}`} label={item.label} hideLabel checked={on} onChecked={() => onChange(toggle(value, item.kind))} />
    </div>;
  })}</div>;
}
