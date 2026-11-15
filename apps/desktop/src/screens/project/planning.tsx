import { useState } from "react";
import { nextGeneration } from "../../api";
import type { Result } from "../../api";
import { planRoadmap, retryResearch } from "../../api-projects";
import type { ProjectContext, ResearchJob } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import { Button, ConfirmDialog } from "../../ui";
import type { Tone } from "../../ui";
import { displayText } from "../conversation";
import { changeOutcome, planningJob, researchJobs } from "./research";
import { RoadmapStatusPanel } from "./roadmap-status";
import { Badge, NotReady } from "./shared";
import type { ProjectData } from "./shared";

/** Where a project's roadmap planning stands: not planned, waiting for research, planning, done,
 * or failed or stopped with its job. */
export type PlanningState =
  | { kind: "idle" }
  | { kind: "waiting" }
  | { kind: "planning"; job: ResearchJob }
  | { kind: "done"; job: ResearchJob }
  | { kind: "failed"; job: ResearchJob };

/** What the planning line shows: the status word and tone, one plain line, and the one action
 * the owner can take, or none while a job is waiting or running. */
export interface PlanningView {
  word: string;
  tone: Tone;
  line: string;
  action: "plan" | "retry" | null;
  report: string | null;
}

/** The label of the action that queues a planning job for any project. */
export const PLAN_NOW = "Plan roadmap now";

const ACTIVE: ResearchJob["status"][] = ["queued", "needs_pool", "running"];
const HELD: ResearchJob["status"][] = ["queued", "needs_pool"];

/** Reads the planning state from the project's job list. The newest planning job decides; the
 * engine holds a queued one back while any research job of the project waits or runs.
 * Input: the jobs. Output: the state. */
export function planningState(jobs: ResearchJob[]): PlanningState {
  const job = planningJob(jobs);
  if (job === null) return { kind: "idle" };
  if (HELD.includes(job.status) && researchJobs(jobs).some((item) => ACTIVE.includes(item.status))) return { kind: "waiting" };
  if (ACTIVE.includes(job.status)) return { kind: "planning", job };
  if (job.status === "done") return { kind: "done", job };
  return { kind: "failed", job };
}

/** Builds the planning line for one state, stating only what the job reports.
 * Input: the state. Output: the word, tone, line, action and the roadmap path once done. */
export function planningView(state: PlanningState): PlanningView {
  switch (state.kind) {
    case "idle": return { word: "Not planned", tone: "neutral", line: "No roadmap yet. Plan one from the brief, research and code.", action: "plan", report: null };
    case "waiting": return { word: "Waiting for research", tone: "neutral", line: "The roadmap is planned once research finishes.", action: null, report: null };
    case "planning": {
      const line = state.job.status === "needs_pool" ? state.job.error?.trim() || "Waiting for a pool"
        : state.job.status === "queued" ? state.job.error?.trim() || "Waiting to start" : "Writing the roadmap and release process";
      return { word: "Planning", tone: "info", line, action: null, report: null };
    }
    case "done": return { word: "Done", tone: "ok", line: "The roadmap and release process are in Context.", action: "plan", report: state.job.report_path };
    case "failed": return state.job.status === "cancelled"
      ? { word: "Stopped", tone: "neutral", line: "Planning stopped before it finished.", action: "retry", report: null }
      : { word: "Failed", tone: "danger", line: state.job.error?.trim() || "Planning stopped with an error.", action: "retry", report: null };
  }
}

/** The confirm line before a planning run that can replace the owner's roadmap documents. */
export const REPLACE_LINE = "Planning writes roadmap.md and release-process.md again from the brief and research. Your edits to them are lost.";

/** Whether a planning run can replace roadmap documents the owner may have edited: a plan ran
 * before, the context lists one of them, or the context isn't read. A roadmap linked from the
 * repository is never written by planning, so it does not count. Inputs: the state and the
 * context read. Output: true when the owner confirms first. */
export function planReplaces(state: PlanningState, context: Result<ProjectContext> | undefined): boolean {
  return state.kind !== "idle" || context?.kind !== "ok" || context.value.items.some((item) => item.kind === "plan" && item.linked !== true);
}

/** Sends the planning line's action to the engine: Retry runs the failed or stopped planning job
 * again and Plan roadmap now queues a new one. Inputs: the project id and the state. Output: the
 * engine result, or null when the state offers no action. */
export function planningCall(id: string, state: PlanningState): Promise<Result<IntentReceipt>> | null {
  const action = planningView(state).action;
  if (action === "retry" && state.kind === "failed") return retryResearch(id, state.job.id, nextGeneration());
  if (action === "plan") return planRoadmap(id, nextGeneration());
  return null;
}

/** The planning line as a view of its state: the status badge, the line, Open roadmap once done
 * and the one action. Inputs: the view, whether a call is running, the last problem and the
 * handlers. Output: the section. */
export function PlanningLineView({ view, busy, problem, onAction, onOpen }: {
  view: PlanningView; busy: boolean; problem: string; onAction: () => void; onOpen: (path: string) => void;
}) {
  return <section className="pj-stack-s" aria-labelledby="pj-roadmap"><h2 id="pj-roadmap">Roadmap</h2>
    <div className="pj-research-row">
      <strong className="pj-kind">Roadmap and release process</strong>
      <Badge word={view.word} tone={view.tone} />
      <span className="pj-secondary">{displayText(view.line)}</span>
      <span>{view.report && <button type="button" className="btn" onClick={() => onOpen(view.report!)}>Open roadmap</button>}
        {view.action && <Button pending={busy} pendingLabel={view.action === "retry" ? "Retrying..." : "Queueing..."} onClick={onAction}>{view.action === "retry" ? "Retry" : PLAN_NOW}</Button>}</span>
    </div>
    {problem && <p className="pj-error" role="alert">{displayText(problem)}</p>}
  </section>;
}

/** The planning line on the project Overview and in Project Settings, General: reads the state
 * from the project's jobs, and runs Retry or Plan roadmap now through the engine, asking first
 * when the run can replace the roadmap documents. Under it sits the roadmap status, or in
 * Project Settings the setting that says when it is checked.
 * Inputs: the project data and whether this is Project Settings. Output: the section, or a
 * plain line while jobs can't be read. */
export function PlanningLine({ data, settings = false }: { data: ProjectData; settings?: boolean }) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [asking, setAsking] = useState(false);
  if (data.research?.kind === "not_ready") return <NotReady what="roadmap planning isn't in this build." />;
  if (data.research?.kind !== "ok") return null;
  const state = planningState(data.research.value.jobs);
  async function act() {
    const call = busy ? null : planningCall(data.id, state);
    if (call === null) return;
    setBusy(true);
    setProblem("");
    const outcome = changeOutcome(await call, "Roadmap planning is queued.", "planning the roadmap");
    setBusy(false);
    setProblem(outcome.note);
    if (outcome.toast) data.notify(outcome.toast);
    if (outcome.reload) data.reload();
  }
  return <>
    <PlanningLineView view={planningView(state)} busy={busy} problem={problem}
      onAction={() => { if (planReplaces(state, data.context)) setAsking(true); else void act(); }}
      onOpen={(path) => data.navigate(`/projects/${encodeURIComponent(data.id)}/context?file=${encodeURIComponent(path)}`)} />
    <RoadmapStatusPanel data={data} settings={settings} />
    <ConfirmDialog open={asking} title="Replace the roadmap?" confirmLabel="Replace" pending={busy}
      onConfirm={() => { setAsking(false); void act(); }} onCancel={() => setAsking(false)}>{REPLACE_LINE}</ConfirmDialog>
  </>;
}
