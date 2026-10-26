import { checkUnconfirmed, harnessCatalog, nextGeneration, poolSummaries, receiptResult } from "../../api";
import type { HarnessEntry, PoolSummary, Result } from "../../api";
import { useContext, useEffect, useState } from "react";
import {
  assignPool, failureCopy, loadPools, loadProjectCatalog, loadProjects, needsReconcile, pickDocuments, receiptState, settledReceipt, setupProject,
} from "../../api-projects";
import type { PoolChoice, ProjectCatalog } from "../../api-projects";
import { SessionContext } from "../../state";
import { Button, ConfirmDialog, Icon, Notice, PageHeader } from "../../ui";
import { displayText } from "../conversation";
import { flash, go } from "../project/flash";
import { homePath } from "../settings/harnesses";
import { NotReady } from "../project/shared";
import { PoolPicker } from "./pool-picker";
import { BasicsFields, ResearchSwitches } from "./fields";
import { TeamEditor } from "./team-editor";
import {
  STEPS, addDocuments, basicsErrors, buildRequest, defaultTeam, documentType, emptyDraft, poolName, preselectPool,
  stepComplete, summary, teamValid,
} from "./wizard";
import type { ProjectDraft, StepId } from "./wizard";
import "../project/projects.css";

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
  const [summaries, setSummaries] = useState<PoolSummary[]>([]);
  const [names, setNames] = useState<string[]>([]);
  const [teamEdited, setTeamEdited] = useState(Boolean(kept?.draft.team));
  const [docErrors, setDocErrors] = useState<string[]>([]);
  const [docPath, setDocPath] = useState("");
  const [picker, setPicker] = useState<"ready" | "missing" | "failed">("ready");
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
    void Promise.all([loadPools(), poolSummaries()]).then(([result, summaries]) => {
      setPools(result);
      const ready = summaries.kind === "ok" ? summaries.value : [];
      setSummaries(ready);
      if (result.kind === "ok" && !kept?.draft.poolChosen) {
        setDraftState((prior) => {
          const next = { ...prior, pool: preselectPool(result.value, ready) };
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
      setCreateError(result.kind === "not_ready" ? "Not ready yet: creating a project with research isn't in this build." : `Couldn't create the project. Your answers are kept. ${failureCopy(result)}`);
      return;
    }
    let receipt = result.value;
    if (needsReconcile(receipt)) {
      const settled = await checkUnconfirmed();
      session?.noteResult(settled.kind === "ok" ? receiptResult(settled.value) : settled);
      receipt = settledReceipt(receipt, settled);
    }
    const state = receiptState(receipt);
    const id = receipt.project_id;
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
      setPicker("missing");
      return;
    }
    if (result.kind === "error") {
      setPicker("failed");
      setDocErrors([failureCopy(result)]);
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

  return <div className="pj pj-new">
    <PageHeader title="New project" actions={<Button variant="quiet" onClick={() => setDiscarding(true)}>Cancel</Button>} />
    {catalog?.kind === "not_ready" && <NotReady what="creating a project with research isn't in this build of the engine. You can fill in the steps; Create project stays off." />}
    {catalog?.kind === "error" && <p className="pj-error" role="alert">{failureCopy(catalog)}</p>}
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
          <BasicsFields prefix="np" draft={draft} onChange={setDraft} catalog={catalog} errors={basics} showErrors={showBasicsErrors} autoFocus />
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
            {picker === "missing" && <NotReady what="the file picker isn't in this build. Add a file by its full path below." />}
          </div>
          {picker !== "ready" && <div className="pj-field">
            <label htmlFor="np-doc-path">Full path of a file on this Mac</label>
            <div className="pj-row">
              <input className="pj-input" id="np-doc-path" value={docPath} onChange={(event) => setDocPath(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addPath(); } }} />
              <button type="button" className="btn" disabled={!docPath.trim() || !catalogValue} onClick={addPath}>Add file</button>
            </div>
          </div>}
          {docErrors.map((error) => <p className="pj-error" role="alert" key={error}>{displayText(error)}</p>)}
          {draft.documents.length > 0 ? <ul className="pj-files" aria-label="Attached documents">{draft.documents.map((document) => <li key={document.path}>
            <span>{displayText(document.name)}</span><span className="pj-small pj-secondary">{documentType(document.name)}</span><span className="pj-small pj-muted pj-mono" title={document.path}>{displayText(homePath(document.path))}</span>
            <button type="button" className="btn quiet" aria-label={`Remove ${document.name}`} onClick={() => setDraft({ ...draft, documents: draft.documents.filter((item) => item.path !== document.path) })}>Remove</button>
          </li>)}</ul> : <p className="pj-muted">No documents yet. Documents are optional.</p>}
        </div>}
        {step === "work" && <div className="pj-stack">
          <h2 id="pj-step-title">What should agents start on?</h2>
          <p className="pj-secondary">Each one starts a research agent as soon as the project is created. Results land in the project folder as Markdown with links and sources.</p>
          <ResearchSwitches prefix="np" catalog={catalog} value={draft.research} onChange={(research) => setDraft({ ...draft, research })} />
        </div>}
        {step === "pool" && <div className="pj-stack">
          <h2 id="pj-step-title">Which accounts should this project use?</h2>
          {pools === undefined && <p className="pj-muted" role="status">Loading pools...</p>}
          {pools?.kind === "not_ready" && <NotReady what="pools aren't in this build." />}
          {pools?.kind === "error" && <p className="pj-error" role="alert">Couldn't load your pools. {failureCopy(pools)}</p>}
          {poolValue && poolValue.length === 0 && <div className="pj-note"><p>You don't have a pool yet.</p>
            <button type="button" className="btn" onClick={() => navigate("/settings/pools?action=new")}>Create pool</button></div>}
          {poolValue && <PoolPicker name="np-pool" pools={poolValue} summaries={summaries} value={draft.pool} later onChange={(pool) => setDraft({ ...draft, pool, poolChosen: true })} />}
          {poolValue && poolValue.length > 0 && <div className="pj-stack-s">
            <Button variant="quiet" icon="plus" onClick={() => navigate("/settings/pools?action=new")}>New pool</Button>
            <span className="pj-help">Your answers here are kept. Come back with New project in the sidebar.</span>
          </div>}
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
