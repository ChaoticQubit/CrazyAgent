import { harnessCatalog, poolSummaries } from "../../api";
import type { HarnessEntry, PoolSummary, Result } from "../../api";
import { useContext, useEffect, useRef, useState } from "react";
import { dismissIntake, newProjectCopy, stopIntake } from "../../api-intake";
import {
  failureCopy, loadPools, loadProjectCatalog, loadProjects, pickDocuments, startSetupJob,
} from "../../api-projects";
import type { PoolChoice, ProjectCatalog, ProjectSetup, ProjectSpec } from "../../api-projects";
import type { IntakeJob } from "../../engine-client";
import { SessionContext } from "../../state";
import { Button, ConfirmDialog, Icon, Notice, PageHeader } from "../../ui";
import { displayText } from "../conversation";
import { jobActions, jobLine, setupFollow } from "../intake/intake";
import { flash, go } from "../project/flash";
import { homePath } from "../settings/harnesses";
import { NotReady } from "../project/shared";
import { AccessLevels } from "../project/access";
import { carryFollowUps, dropStarted, keepStarted, startedJob, type Carry } from "./carry";
import { PoolPicker } from "./pool-picker";
import { BasicsFields, PublishFields, ResearchSwitches, RoadmapSwitch, useGitHubPublish } from "./fields";
import { noteLostStart } from "../project/publish-job";
import { createWait, emptyPublish, publishOutcome, startPublish, startReplyLost } from "./publish";
import { TeamEditor } from "./team-editor";
import {
  STEPS, addDocuments, basicsErrors, buildRequest, defaultTeam, documentType, draftAccess, emptyDraft, levelUnavailable, poolName, preselectPool,
  stepComplete, summary, teamValid,
} from "./wizard";
import type { ProjectDraft, StepId } from "./wizard";
import "../project/projects.css";

let kept: { draft: ProjectDraft; step: StepId } | null = null;

/** Forgets the kept New Project draft and the job it started, whose follow-ups then never run
 * unless they already started. Input: none. Output: none. */
export function discardKeptDraft(): void {
  kept = null;
  dropStarted();
}

function valueOf<T>(result: Result<T> | undefined): T | null {
  return result?.kind === "ok" ? result.value : null;
}

const KEPT_LINE = "Couldn't create the project. Your answers are kept.";

/** What Create got from the engine: the started New Project job, or the note the page shows
 * with the answers kept. */
export type SetupStarted = { kind: "started"; job: IntakeJob } | { kind: "refused"; note: string };

/** Starts the engine's New Project job (DEC-177 add.2), which may wait its turn behind other
 * jobs (DEC-197). Inputs: the name spec, the setup and the start. Output: the started job, or
 * the note the page shows. */
export async function setupStart(spec: ProjectSpec, setup: ProjectSetup, start = startSetupJob): Promise<SetupStarted> {
  const result = await start(spec, setup);
  if (result.kind === "ok") return { kind: "started", job: result.value };
  if (result.kind === "not_ready") return { kind: "refused", note: "Not ready yet: creating a project with research isn't in this build." };
  return { kind: "refused", note: `${KEPT_LINE} ${newProjectCopy(result)}` };
}

/** What New Project opened again keeps of the job it started before the owner left it
 * (DEC-177 add.2 Q1 A): it follows one still building and keeps the answers of one that failed
 * or stopped; a project the job made keeps no answers, read from the job ended done or, once
 * the job left Home, from the projects listing it (rv-code bgjobs-3b F3). Inputs: the jobs read,
 * the started job and the project ids the session lists. Output: the job id to follow, or null,
 * and whether the answers stay. */
export function reopened(jobs: readonly IntakeJob[], started: { id: string; project: string }, projects: readonly string[]): { follow: string | null; keep: boolean } {
  const now = setupFollow(jobs, started.id);
  switch (now.kind) {
    case "running":
      return { follow: started.id, keep: true };
    case "done":
      return { follow: null, keep: false };
    case "ended":
      return { follow: null, keep: true };
    case "lost":
      return { follow: null, keep: !projects.includes(started.project) };
  }
}

/** Reads the New Project job this page started before the owner left it, as the page opens
 * again, by `reopened`: it follows one still building, else lets go, dropping the answers of a
 * project already made, whose follow-ups Home's poll ran, Home saying how it ended. Inputs:
 * Home's read of the engine's jobs and the project ids the session lists. Output: the job id to
 * follow, or null. */
function resumeSetup(jobs: readonly IntakeJob[], projects: readonly string[]): string | null {
  const started = startedJob();
  if (started === null) return null;
  const now = reopened(jobs, started, projects);
  if (now.follow !== null) return now.follow;
  if (!now.keep) kept = null;
  dropStarted();
  return null;
}

/** The five-step New Project flow: Basics, Documents, Work, Pool and Team, then Create project.
 * Inputs: an optional starting step and a navigate handler for hash routes.
 * Output: the page; answers are kept if the owner leaves and comes back. */
export function NewProjectFlow({ step: routeStep, navigate = go }: { step?: string; navigate?: (path: string) => void }) {
  const session = useContext(SessionContext);
  const [following, setFollowing] = useState<string | null>(() => resumeSetup(session?.intake ?? [],
    session?.projects.kind === "ok" ? session.projects.value.map((row) => row.id) : []));
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
  const [starting, setStarting] = useState(false);
  const [finishing, setFinishing] = useState(false);
  const [createError, setCreateError] = useState("");
  const [discarding, setDiscarding] = useState(false);
  const mounted = useRef(true);
  const follow = following === null ? null : setupFollow(session?.intake ?? [], following);
  const creating = starting || finishing || following !== null;
  const github = useGitHubPublish();

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

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    session?.followIntake(following);
    return () => session?.followIntake(null);
  }, [following]);

  useEffect(() => {
    if (following === null || follow === null || follow.kind === "running") return;
    const carried = follow.kind === "done" ? carryFollowUps(following, follow.project) : null;
    dropStarted();
    setFollowing(null);
    if (follow.kind === "done") {
      if (carried) void finish(follow.project, following, carried);
      return;
    }
    if (follow.kind === "ended") void leave(following);
    setCreateError(follow.kind === "ended" ? `${KEPT_LINE} ${follow.words}` : "We couldn't confirm the project was created. Check your projects before trying again.");
  }, [following, follow?.kind]);

  const catalogValue = valueOf(catalog);
  const harnessValue = valueOf(harnesses);
  const poolValue = valueOf(pools);
  const chosenPool = poolValue?.find((choice) => choice.pool.id === draft.pool) ?? null;
  const basics = basicsErrors(draft, names);
  const index = STEPS.findIndex((item) => item.id === step);
  const showBasicsErrors = visited.has("documents") || visited.has("work") || visited.has("pool") || visited.has("team") || draft.name !== "";
  const teamOk = draft.team !== null && teamValid(draft.team, harnessValue);
  const publishWait = createWait(draft.publish ?? emptyPublish(), draft.name, draft.about, github.offered, github.signedIn, github.checking);
  const canCreate = catalog?.kind === "ok" && Object.keys(basics).length === 0 && !publishWait && teamOk && !creating;

  async function create() {
    if (!canCreate) return;
    setStarting(true);
    setCreateError("");
    const request = buildRequest(draft);
    const result = await setupStart(request.spec, request.setup);
    setStarting(false);
    if (result.kind === "refused") {
      setCreateError(result.note);
      return;
    }
    keepStarted({
      id: result.job.id, project: result.job.project_id ?? "", draft, name: request.spec.name, pool: draft.pool,
      poolLabel: chosenPool ? poolName(chosenPool.pool) : draft.pool ?? "", access: draftAccess(draft, catalogValue?.agent_access),
      research: request.setup.research.length,
    });
    session?.refreshIntake(result.job);
    setFollowing(result.job.id);
  }

  async function stop(job: IntakeJob) {
    const result = await stopIntake(job.id);
    if (result.kind === "error") setCreateError(newProjectCopy(result));
    session?.refreshIntake();
  }

  /** Takes the page's ended job off Home: waits for the engine to drop it, then reads the job
   * again, so Home's panel clears at once instead of at the idle read. Input: the job's id. */
  async function leave(job: string) {
    await dismissIntake(job);
    session?.refreshIntake();
  }

  /** Ends a New Project on its page once its job is done: the shared pool and agent access
   * follow-ups (DEC-177 add.2 Q1 B), which finish even if the owner leaves, a toast then saying
   * how they ended, then, only while the page stays open, its publish, the job leaving Home and
   * the project opening. Inputs: the new project's id, the job's id and the carry. */
  async function finish(id: string, job: string, carried: Carry) {
    kept = null;
    setFinishing(true);
    const draft = carried.setup.draft;
    let message = await carried.outcome;
    if (!mounted.current) {
      session?.showToast(message, { name: "home" });
      return;
    }
    const publishing = startPublish(id, draft.publish ?? emptyPublish(), draft.name, draft.about, github.offered);
    const started = publishing ? await publishing : null;
    const outcome = publishOutcome(started);
    if (startReplyLost(started)) noteLostStart(id, Date.now());
    if (outcome) message = `${message} ${outcome}`;
    void leave(job);
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
          <PublishFields prefix="np" draft={draft} onChange={setDraft} github={github} />
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
          <RoadmapSwitch prefix="np" value={draft.roadmap} onChange={(roadmap) => setDraft({ ...draft, roadmap })} />
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
          {harnesses?.kind === "not_ready" && <NotReady what="the harness list isn't in this build, so installed status isn't checked." />}
          {draft.team ? <TeamEditor prefix="np" team={draft.team} harnesses={harnessValue} pool={chosenPool} research={draft.research}
            onChange={(team) => { setTeamEdited(true); setDraft({ ...draft, team }); }} />
            : catalog === undefined ? <p className="pj-muted" role="status">Loading team defaults...</p> : <NotReady what="team defaults come from the engine, which isn't ready in this build." />}
          {teamEdited && catalogValue && <button type="button" className="btn quiet" onClick={() => { setTeamEdited(false); setDraft({ ...draft, team: defaultTeam(catalogValue.default_team, harnessValue) }); }}>Use smart defaults</button>}
          <AccessLevels name="np-access" value={draftAccess(draft, catalogValue?.agent_access)} unavailable={(level) => levelUnavailable(catalogValue?.agent_access, level)} gaps={(level) => catalogValue?.agent_access?.find((item) => item.access === level)?.gaps ?? []} onChange={(access) => setDraft({ ...draft, access })} />
          <p className="pj-help">You can change agent access later in the project's Settings, Agent access.</p>
          <p className="pj-summary">{summary(draft, chosenPool ? poolName(chosenPool.pool) : null)}</p>
        </div>}
        {createError && <Notice tone="danger" alert>{createError}</Notice>}
        {follow?.kind === "running" && <div className="pj-row" role="status">
          <span className="pj-small pj-secondary">{displayText(jobLine(follow.job))}</span>
          {jobActions(follow.job).includes("stop") && <Button size="small" variant="secondary" onClick={() => void stop(follow.job)}>Stop</Button>}
          <span className="pj-small pj-muted">You can leave this page and it keeps going. If you do, choose its pool and agent access in its Settings afterwards.</span>
        </div>}
        <footer className="pj-footer">
          <Button icon="arrow-left" disabled={index === 0} onClick={() => setStep(STEPS[Math.max(0, index - 1)].id)}>Back</Button>
          {step === "team"
            ? <Button type="submit" variant="primary" icon="folder-plus" pending={creating} pendingLabel="Creating..." disabled={!canCreate} aria-describedby={!canCreate ? "np-create-why" : undefined}>Create project</Button>
            : <Button type="submit" variant="primary">Next</Button>}
          {step === "team" && !canCreate && !creating && <span className="pj-small pj-muted" id="np-create-why">{catalog?.kind !== "ok" ? "Create project needs the engine's project setup." : Object.keys(basics).length ? "Finish Basics first." : publishWait ?? "Fix the team above first."}</span>}
        </footer>
      </form>
    </div>
    <ConfirmDialog open={discarding} title="Discard this new project?" confirmLabel="Discard" cancelLabel="Keep editing"
      onCancel={() => setDiscarding(false)} onConfirm={() => { discardKeptDraft(); setDiscarding(false); navigate("/home"); }}>
      Your answers on every step are thrown away.
    </ConfirmDialog>
  </div>;
}
