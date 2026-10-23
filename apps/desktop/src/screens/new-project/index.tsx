import { harnessCatalog, nextGeneration } from "../../api";
import type { HarnessEntry, Result } from "../../api";
import { useContext, useEffect, useState } from "react";
import { assignPool, failureCopy, loadPools, loadProjectCatalog, loadProjects, pickDocuments, receiptState, setupProject } from "../../api-projects";
import type { Platform, PoolChoice, ProjectCatalog, ResearchKind } from "../../api-projects";
import { SessionContext } from "../../state";
import { Button, ConfirmDialog, Icon, Notice, PageHeader, Switch } from "../../ui";
import { displayText } from "../conversation";
import { flash, go } from "../project/flash";
import { NotReady } from "../project/shared";
import { PoolPicker } from "./pool-picker";
import { RESEARCH_ICONS, RESEARCH_LABELS, TeamEditor } from "./team-editor";
import {
  RESEARCH_COPY, STEPS, addDocuments, addStack, basicsErrors, buildRequest, defaultTeam, emptyDraft, poolName, preselectPool,
  removeStack, stepComplete, summary, teamValid,
} from "./wizard";
import type { ProjectDraft, StepId } from "./wizard";
import "../project/projects.css";

const PLATFORMS: { kind: Platform; label: string }[] = [
  { kind: "website", label: "Website" },
  { kind: "mobile_app", label: "Mobile app" },
  { kind: "desktop_app", label: "Desktop app" },
  { kind: "cli", label: "CLI" },
  { kind: "api_backend", label: "API or backend" },
  { kind: "other", label: "Other" },
];

const GROUP_LABELS = { language: "Languages", framework: "Frameworks", database: "Databases" } as const;

let kept: { draft: ProjectDraft; step: StepId } | null = null;

/** Forgets the kept New Project draft.
 * Input: none. Output: none. */
export function discardKeptDraft(): void {
  kept = null;
}

function valueOf<T>(result: Result<T> | undefined): T | null {
  return result?.kind === "ok" ? result.value : null;
}

/** The five-step New Project flow: Basics, Documents, Work, Pool and Team, then Create project.
 * Inputs: an optional starting step and a navigate handler for hash routes.
 * Output: the page; answers are kept if the owner leaves and comes back. */
export function NewProjectFlow({ step: routeStep, navigate = go }: { step?: string; navigate?: (path: string) => void }) {
  const initialStep = STEPS.some((item) => item.id === routeStep) ? routeStep as StepId : kept?.step ?? "basics";
  const [draft, setDraftState] = useState<ProjectDraft>(() => kept?.draft ?? emptyDraft());
  const [step, setStepState] = useState<StepId>(initialStep);
  const [visited, setVisited] = useState<Set<StepId>>(() => new Set([initialStep]));
  const [catalog, setCatalog] = useState<Result<ProjectCatalog>>();
  const [harnesses, setHarnesses] = useState<Result<HarnessEntry[]>>();
  const [pools, setPools] = useState<Result<PoolChoice[]>>();
  const [names, setNames] = useState<string[]>([]);
  const [teamEdited, setTeamEdited] = useState(Boolean(kept?.draft.team));
  const [stackQuery, setStackQuery] = useState("");
  const [stackError, setStackError] = useState("");
  const [docErrors, setDocErrors] = useState<string[]>([]);
  const [docPath, setDocPath] = useState("");
  const [pickerReady, setPickerReady] = useState(true);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState("");
  const [discarding, setDiscarding] = useState(false);
  const session = useContext(SessionContext);

  const setDraft = (next: ProjectDraft) => {
    setDraftState(next);
    kept = { draft: next, step };
  };
  const setStep = (next: StepId) => {
    setStepState(next);
    setVisited((prior) => new Set(prior).add(next));
    kept = { draft, step: next };
    if (next !== routeStep) navigate(`/projects/new?step=${next}`);
  };

  useEffect(() => {
    if (routeStep && routeStep !== step && STEPS.some((item) => item.id === routeStep)) {
      const next = routeStep as StepId;
      setStepState(next);
      setVisited((prior) => new Set(prior).add(next));
      kept = { draft, step: next };
    }
  }, [routeStep]);

  useEffect(() => {
    void harnessCatalog().then(setHarnesses);
    void loadProjects().then((result) => { if (result.kind === "ok") setNames(result.value.map((item) => item.name)); });
    void loadPools().then((result) => {
      setPools(result);
      if (result.kind === "ok" && !kept?.draft.poolChosen) {
        setDraftState((prior) => {
          const next = { ...prior, pool: preselectPool(result.value) };
          kept = { draft: next, step: kept?.step ?? step };
          return next;
        });
      }
    });
  }, []);

  useEffect(() => {
    let live = true;
    void loadProjectCatalog(draft.pool, draft.research).then((result) => {
      if (!live) return;
      setCatalog(result);
      if (result.kind === "ok" && !teamEdited) {
        setDraftState((prior) => {
          const next = { ...prior, team: defaultTeam(result.value.default_team, valueOf(harnesses)) };
          kept = { draft: next, step: kept?.step ?? "basics" };
          return next;
        });
      }
    });
    return () => { live = false; };
  }, [draft.pool, draft.research.join(","), harnesses?.kind]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setDiscarding(!discarding);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [discarding]);

  const catalogValue = valueOf(catalog);
  const harnessValue = valueOf(harnesses);
  const poolValue = valueOf(pools);
  const chosenPool = poolValue?.find((choice) => choice.pool.id === draft.pool) ?? null;
  const basics = basicsErrors(draft, names);
  const index = STEPS.findIndex((item) => item.id === step);
  const showBasicsErrors = visited.has("documents") || visited.has("work") || visited.has("pool") || visited.has("team") || draft.name !== "";
  const teamOk = draft.team !== null && teamValid(draft.team, harnessValue);
  const canCreate = catalog?.kind === "ok" && Object.keys(basics).length === 0 && teamOk && !creating;

  async function create() {
    if (!canCreate) return;
    setCreating(true);
    setCreateError("");
    const request = buildRequest(draft);
    const result = await setupProject(request.spec, request.setup, nextGeneration());
    if (result.kind !== "ok") {
      setCreating(false);
      setCreateError(result.kind === "not_ready" ? "Not ready yet: creating a project with research isn't in this build." : `Couldn't create the project. Your answers are kept. ${failureCopy(result.code)}`);
      return;
    }
    const state = receiptState(result.value);
    const id = result.value.project_id;
    if (state.kind !== "applied" || !id) {
      setCreating(false);
      setCreateError(state.kind === "unconfirmed" ? "We couldn't confirm your last change. Check your projects before trying again." : `Couldn't create the project. Your answers are kept. ${failureCopy(state.kind === "rejected" ? state.code : "unknown")}`);
      return;
    }
    const name = request.spec.name;
    const count = request.setup.research.length;
    let message = draft.pool
      ? `${name} created.${count ? ` ${count} research ${count === 1 ? "agent is" : "agents are"} starting.` : ""}`
      : `${name} created. Research starts when you choose a pool.`;
    if (draft.pool) {
      const assigned = await assignPool(id, draft.pool, nextGeneration());
      if (assigned.kind !== "ok" || assigned.value.kind !== "applied") {
        message = `${name} created. Couldn't assign ${chosenPool ? poolName(chosenPool.pool) : draft.pool}. Choose it again in the project's Settings, Pool.`;
      }
    }
    kept = null;
    await session?.refreshProjects();
    flash(message);
    navigate(`/projects/${encodeURIComponent(id)}/overview`);
  }

  function next() {
    if (index < STEPS.length - 1) setStep(STEPS[index + 1].id);
  }

  async function choose() {
    const result = await pickDocuments();
    if (result.kind === "not_ready") {
      setPickerReady(false);
      return;
    }
    if (result.kind === "error") {
      setDocErrors([failureCopy(result.code)]);
      return;
    }
    const added = addDocuments(draft.documents, result.value, catalogValue?.document_extensions ?? [], catalogValue?.documents_max ?? 16);
    setDraft({ ...draft, documents: added.documents });
    setDocErrors(added.errors);
  }

  function addPath() {
    const added = addDocuments(draft.documents, [docPath.trim()], catalogValue?.document_extensions ?? [], catalogValue?.documents_max ?? 16);
    setDraft({ ...draft, documents: added.documents });
    setDocErrors(added.errors);
    if (added.errors.length === 0) setDocPath("");
  }

  const toggle = <T,>(list: T[], value: T) => list.includes(value) ? list.filter((item) => item !== value) : [...list, value];
  const stackOptions = (catalogValue?.tech_stack ?? []).filter((option) => !stackQuery.trim() || option.label.toLowerCase().includes(stackQuery.trim().toLowerCase()));
  const research = catalogValue?.research ?? (Object.keys(RESEARCH_LABELS) as ResearchKind[]).map((kind) => ({ kind, label: RESEARCH_LABELS[kind] }));
  const platforms = catalogValue?.platforms ?? PLATFORMS;

  return <div className="pj pj-new">
    <PageHeader title="New project" actions={<Button variant="quiet" onClick={() => setDiscarding(true)}>Cancel</Button>} />
    {catalog?.kind === "not_ready" && <NotReady what="creating a project with research isn't in this build of the engine. You can fill in the steps; Create project stays off." />}
    {catalog?.kind === "error" && <p className="pj-error" role="alert">{failureCopy(catalog.code)}</p>}
    <div className="pj-new-body">
      <nav className="pj-step-rail" aria-label="Steps">
        <ol>{STEPS.map((item, position) => {
          const done = visited.has(item.id) && item.id !== step && stepComplete(item.id, draft, names, harnessValue);
          return <li key={item.id}>
            <button type="button" aria-current={item.id === step ? "step" : undefined} className={item.id === step ? "current" : ""} onClick={() => setStep(item.id)}>
              <span className={`pj-step-dot ${done ? "done" : ""}`} aria-hidden="true">{done ? <Icon name="check" size={13} /> : position + 1}</span>
              <span>{item.label}</span>{done && <span className="pj-sr"> (complete)</span>}
            </button>
          </li>;
        })}</ol>
      </nav>
      <form className="pj-step-form" aria-labelledby="pj-step-title" onSubmit={(event) => { event.preventDefault(); if (step === "team") void create(); else next(); }}>
        {step === "basics" && <div className="pj-stack">
          <h2 id="pj-step-title">Basics</h2>
          <div className="pj-field">
            <label htmlFor="np-name">Project name</label>
            <input className="pj-input" id="np-name" value={draft.name} maxLength={200} autoFocus aria-invalid={Boolean(showBasicsErrors && basics.name) || undefined}
              aria-describedby={showBasicsErrors && basics.name ? "np-name-error" : undefined} onChange={(event) => setDraft({ ...draft, name: event.target.value })} />
            {showBasicsErrors && basics.name && <p className="pj-error" id="np-name-error" role="alert">{basics.name}</p>}
          </div>
          <div className="pj-field">
            <label htmlFor="np-about">What is it about?</label>
            <textarea className="pj-input" id="np-about" rows={5} value={draft.about} aria-describedby="np-about-help" onChange={(event) => setDraft({ ...draft, about: event.target.value })} />
            <p className="pj-help" id="np-about-help">Who it's for and what it should do. Agents use this for research.{draft.about.length > 3600 ? ` ${draft.about.length} of 4000.` : ""}</p>
            {basics.about && <p className="pj-error" role="alert">{basics.about}</p>}
          </div>
          <fieldset className="pj-field">
            <legend>Tech stack</legend>
            {draft.stack.length > 0 && <div className="pj-chips" aria-label="Picked tech stack">{draft.stack.map((item) => <span className="pj-chip" key={item}>{displayText(item)}
              <button type="button" aria-label={`Remove ${item}`} onClick={() => setDraft({ ...draft, stack: removeStack(draft.stack, item) })}>{"×"}</button></span>)}</div>}
            <div className="pj-row">
              <label className="pj-sr" htmlFor="np-stack-search">Search or add a tech stack item</label>
              <input className="pj-input" id="np-stack-search" value={stackQuery} aria-describedby="np-stack-help" onChange={(event) => { setStackQuery(event.target.value); setStackError(""); }}
                onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); const result = addStack(draft.stack, stackQuery); setStackError(result.error ?? ""); if (!result.error) { setDraft({ ...draft, stack: result.stack }); setStackQuery(""); } } }} />
              <button type="button" className="btn" disabled={!stackQuery.trim()} onClick={() => { const result = addStack(draft.stack, stackQuery); setStackError(result.error ?? ""); if (!result.error) { setDraft({ ...draft, stack: result.stack }); setStackQuery(""); } }}>Add custom</button>
            </div>
            <p className="pj-help" id="np-stack-help">Pick from the list or add your own.</p>
            {stackError && <p className="pj-error" role="alert">{stackError}</p>}
            {catalogValue ? (["language", "framework", "database"] as const).map((group) => {
              const options = stackOptions.filter((option) => option.group === group);
              return options.length > 0 && <div className="pj-stack-group" key={group}><span className="pj-small pj-muted">{GROUP_LABELS[group]}</span>
                <div className="pj-chips">{options.map((option) => {
                  const on = draft.stack.some((item) => item.toLowerCase() === option.label.toLowerCase());
                  return <button type="button" key={option.id} className={`pj-chip-toggle ${on ? "on" : ""}`} aria-pressed={on}
                    onClick={() => setDraft({ ...draft, stack: on ? draft.stack.filter((item) => item.toLowerCase() !== option.label.toLowerCase()) : addStack(draft.stack, option.label).stack })}>{option.label}</button>;
                })}</div></div>;
            }) : catalog === undefined ? <p className="pj-help" role="status">Loading the tech stack list...</p>
              : <p className="pj-help">The tech stack list isn't available. Add your own entries.</p>}
          </fieldset>
          <fieldset className="pj-field">
            <legend>Target platforms</legend>
            <div className="pj-chips">{platforms.map((platform) => <label className={`pj-chip-toggle ${draft.platforms.includes(platform.kind) ? "on" : ""}`} key={platform.kind}>
              <input type="checkbox" className="pj-sr" checked={draft.platforms.includes(platform.kind)} onChange={() => setDraft({ ...draft, platforms: toggle(draft.platforms, platform.kind) })} />{platform.label}</label>)}</div>
            {draft.platforms.includes("other") && <div className="pj-field">
              <label htmlFor="np-other">Other platform</label>
              <input className="pj-input" id="np-other" value={draft.otherPlatform} onChange={(event) => setDraft({ ...draft, otherPlatform: event.target.value })} />
              {basics.otherPlatform && <p className="pj-error" role="alert">{basics.otherPlatform}</p>}
            </div>}
          </fieldset>
          <div className="pj-field">
            <span className="pj-label">Project folder</span>
            <p>CrazyAgent creates a new folder for this project when you create it.</p>
            <p className="pj-help">Agents can only write inside this folder.</p>
          </div>
        </div>}
        {step === "documents" && <div className="pj-stack">
          <h2 id="pj-step-title">Documents</h2>
          <div className="pj-drop">
            <p><Button icon="upload" onClick={() => void choose()}>Choose files</Button></p>
            <p className="pj-help">PDF, Markdown, text, Word, CSV, PNG and JPG. Up to 50 MB each, 200 MB per project. Files are copied into context/documents in the project folder.</p>
            {!pickerReady && <NotReady what="the file picker isn't in this build. Add a file by its full path below." />}
          </div>
          {!pickerReady && <div className="pj-field">
            <label htmlFor="np-doc-path">Full path of a file on this Mac</label>
            <div className="pj-row">
              <input className="pj-input" id="np-doc-path" value={docPath} onChange={(event) => setDocPath(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addPath(); } }} />
              <button type="button" className="btn" disabled={!docPath.trim() || !catalogValue} onClick={addPath}>Add file</button>
            </div>
          </div>}
          {docErrors.map((error) => <p className="pj-error" role="alert" key={error}>{displayText(error)}</p>)}
          {draft.documents.length > 0 ? <ul className="pj-files" aria-label="Attached documents">{draft.documents.map((document) => <li key={document.path}>
            <span>{displayText(document.name)}</span><span className="pj-small pj-muted pj-mono">{displayText(document.path)}</span>
            <button type="button" className="btn quiet" aria-label={`Remove ${document.name}`} onClick={() => setDraft({ ...draft, documents: draft.documents.filter((item) => item.path !== document.path) })}>Remove</button>
          </li>)}</ul> : <p className="pj-muted">No documents yet. Documents are optional.</p>}
        </div>}
        {step === "work" && <div className="pj-stack">
          <h2 id="pj-step-title">What should agents start on?</h2>
          <p className="pj-secondary">Each one starts a research agent as soon as the project is created. Results land in the project folder as Markdown with links and sources.</p>
          <div className="pj-switch-list">{research.map((item) => {
            const on = draft.research.includes(item.kind);
            return <div className="pj-switch-item" key={item.kind}>
              <Icon name={RESEARCH_ICONS[item.kind]} size={18} />
              <span><strong>{item.label}</strong><span className="pj-secondary">{RESEARCH_COPY[item.kind]}</span></span>
              <Switch id={`np-research-${item.kind}`} label={item.label} hideLabel checked={on} onChecked={() => setDraft({ ...draft, research: toggle(draft.research, item.kind) })} />
            </div>;
          })}</div>
        </div>}
        {step === "pool" && <div className="pj-stack">
          <h2 id="pj-step-title">Which accounts should this project use?</h2>
          {pools === undefined && <p className="pj-muted" role="status">Loading pools...</p>}
          {pools?.kind === "not_ready" && <NotReady what="pools aren't in this build." />}
          {pools?.kind === "error" && <p className="pj-error" role="alert">Couldn't load your pools. {failureCopy(pools.code)}</p>}
          {poolValue && poolValue.length === 0 && <div className="pj-note"><p>You don't have a pool yet.</p>
            <button type="button" className="btn" onClick={() => navigate("/settings/pools?action=new")}>Create pool</button></div>}
          {poolValue && <PoolPicker name="np-pool" pools={poolValue} value={draft.pool} later onChange={(pool) => setDraft({ ...draft, pool, poolChosen: true })} />}
        </div>}
        {step === "team" && <div className="pj-stack">
          <h2 id="pj-step-title">Team</h2>
          {createError && <Notice tone="danger" alert>{createError}</Notice>}
          {harnesses?.kind === "not_ready" && <NotReady what="the harness list isn't in this build, so installed status isn't checked." />}
          {draft.team ? <TeamEditor prefix="np" team={draft.team} harnesses={harnessValue} pool={chosenPool} research={draft.research}
            onChange={(team) => { setTeamEdited(true); setDraft({ ...draft, team }); }} />
            : catalog === undefined ? <p className="pj-muted" role="status">Loading team defaults...</p> : <NotReady what="team defaults come from the engine, which isn't ready in this build." />}
          {teamEdited && catalogValue && <button type="button" className="btn quiet" onClick={() => { setTeamEdited(false); setDraft({ ...draft, team: defaultTeam(catalogValue.default_team, harnessValue) }); }}>Use smart defaults</button>}
          <p className="pj-summary">{summary(draft, chosenPool ? poolName(chosenPool.pool) : null)}</p>
        </div>}
        <footer className="pj-footer">
          <Button icon="arrow-left" disabled={index === 0} onClick={() => setStep(STEPS[Math.max(0, index - 1)].id)}>Back</Button>
          {step === "team"
            ? <Button type="submit" variant="primary" icon="folder-plus" pending={creating} pendingLabel="Creating..." disabled={!canCreate} aria-describedby={!canCreate ? "np-create-why" : undefined}>Create project</Button>
            : <Button type="submit" variant="primary">Next</Button>}
          {step === "team" && !canCreate && !creating && <span className="pj-small pj-muted" id="np-create-why">{catalog?.kind !== "ok" ? "Create project needs the engine's project setup." : Object.keys(basics).length ? "Finish Basics first." : "Fix the team above first."}</span>}
        </footer>
      </form>
    </div>
    <ConfirmDialog open={discarding} title="Discard this new project?" confirmLabel="Discard" cancelLabel="Keep editing"
      onCancel={() => setDiscarding(false)} onConfirm={() => { kept = null; setDiscarding(false); navigate("/home"); }}>
      Your answers on every step are thrown away.
    </ConfirmDialog>
  </div>;
}
