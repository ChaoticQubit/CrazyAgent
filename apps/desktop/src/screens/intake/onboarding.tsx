import { useEffect, useState } from "react";
import { nextGeneration, type Result } from "../../api";
import { intakeCopy, loadOnboarding } from "../../api-intake";
import {
  loadProjectCatalog, loadProjects, loadSetup, openLink, receiptState, updateProjectSetup, type ProjectCatalog, type StoredSetup,
} from "../../api-projects";
import type { OnboardingView } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, Notice, NotReady, PageHeader, Panel, Skeleton, StatusBadge, navigate } from "../../ui";
import { BasicsFields, ResearchSwitches } from "../new-project/fields";
import { basicsErrors, editRequest, type ProjectDraft } from "../new-project/wizard";
import { MarkdownView } from "../project/markdown";
import { ONBOARDING_POLL_MS, confirmReady, missingLabels, onboardingDraft, settled, stepView } from "./intake";
import "../project/projects.css";
import "./intake.css";

const STEPS = [{ key: "graph", title: "Build the code graph" }, { key: "mapping", title: "Map the project" }] as const;

/** The setup review as a view of its state: the graph and mapping steps, the mapping agent's
 * overview, the pre-filled form with missing items marked, and a Confirm that waits for the agent.
 * Inputs: the project name, the onboarding read, the form draft or null before it fills in, the
 * catalog, field errors, flags, the last problem and the handlers. Output: the page. */
export function OnboardingReviewView({ name, view, draft, catalog, errors, showErrors, busy, problem, onDraft, onConfirm, onOpenLink, onRetry }: {
  name: string;
  view: Loaded<OnboardingView>;
  draft: ProjectDraft | null;
  catalog: Result<ProjectCatalog> | undefined;
  errors: ReturnType<typeof basicsErrors>;
  showErrors: boolean;
  busy: boolean;
  problem: string;
  onDraft: (draft: ProjectDraft) => void;
  onConfirm: () => void;
  onOpenLink: (href: string) => void;
  onRetry?: () => void;
}) {
  const header = <PageHeader title={`Set up ${name}`} description="Review what CrazyAgent found, fill in anything missing, then confirm." />;
  if (view.kind === "loading") return <div className="pj in-page">{header}<Skeleton shape="line" count={5} /></div>;
  if (view.kind === "not_ready") return <div className="pj in-page">{header}<NotReady what="Reviewing a found project" /></div>;
  if (view.kind === "error") return <div className="pj in-page">{header}<LoadError message={view.message} onRetry={onRetry ?? (() => navigate({ name: "home" }))} /></div>;
  const value = view.value;
  const gate = confirmReady(value);
  const missing = missingLabels(value.missing);
  return <div className="pj in-page">
    {header}
    <ol className="in-list" aria-label="Setup steps">{STEPS.map((step) => {
      const shown = stepView(value[step.key]);
      return <li className="in-row" key={step.key}>
        <span className="in-row-main"><strong>{step.title}</strong>{shown.line && <span className="pj-small pj-error">{shown.line}</span>}</span>
        <StatusBadge word={shown.word} tone={shown.tone} />
      </li>;
    })}</ol>
    {value.overview && <Panel title="Overview"><MarkdownView source={value.overview} onOpenLink={onOpenLink} /></Panel>}
    {draft && missing.length > 0 && <Notice tone="warn">Not found: {missing.join(", ")}. Add them below.</Notice>}
    {draft && <form className="pj-stack" aria-label="Project details" noValidate onSubmit={(event) => { event.preventDefault(); onConfirm(); }}>
      <BasicsFields prefix="ob" draft={draft} onChange={onDraft} catalog={catalog} errors={errors} showErrors={showErrors} missing={value.missing} />
      <fieldset className="pj-field">
        <legend>Research</legend>
        <p className="pj-help">Research is off for existing code. Turn on what you want; it runs on the project's pool.</p>
        <ResearchSwitches prefix="ob" catalog={catalog} value={draft.research} onChange={(research) => onDraft({ ...draft, research })} />
      </fieldset>
    </form>}
    <p className="pj-secondary" role="status">{gate.line}</p>
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <span className="pj-row">
      <Button variant="primary" disabled={!gate.ready || draft === null} pending={busy} pendingLabel="Saving..." onClick={onConfirm}>Confirm and finish setup</Button>
    </span>
  </div>;
}

/** Setup review for a found or cloned project: reads its progress until the mapping agent stops,
 * fills the form from the agent's draft once, and confirms through the project setup change.
 * Input: the project id. */
export function OnboardingReview({ id }: { id: string }) {
  const session = useAppSession();
  const [view, reloadView] = useLoad(() => loadOnboarding(id), [id]);
  const [stored] = useLoad(() => loadSetup(id), [id]);
  const [catalog, setCatalog] = useState<Result<ProjectCatalog>>();
  const [names, setNames] = useState<string[]>([]);
  const [draft, setDraft] = useState<ProjectDraft | null>(null);
  const [showErrors, setShowErrors] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const value = view.kind === "ok" ? view.value : null;
  const saved: StoredSetup | null = stored.kind === "ok" ? stored.value : null;
  const done = value !== null && settled(value);

  useEffect(() => {
    if (view.kind !== "ok" || done) return;
    const timer = window.setTimeout(reloadView, ONBOARDING_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [view, done]);

  useEffect(() => {
    let live = true;
    void loadProjectCatalog(null, []).then((result) => { if (live) setCatalog(result); });
    void loadProjects().then((result) => { if (live && result.kind === "ok") setNames(result.value.filter((project) => project.id !== id).map((project) => project.name)); });
    return () => { live = false; };
  }, [id]);

  useEffect(() => {
    if (draft === null && value !== null && saved !== null && confirmReady(value).ready) setDraft(onboardingDraft(value, saved));
  }, [value, saved, draft]);

  const name = draft?.name.trim() || value?.name?.trim() || saved?.name || "this project";
  const errors = draft ? basicsErrors(draft, names) : {};

  const confirm = async () => {
    if (!draft || !saved || busy) return;
    if (Object.keys(errors).length > 0) { setShowErrors(true); document.getElementById("ob-name")?.focus(); return; }
    const request = editRequest(saved, draft);
    setBusy(true);
    setProblem("");
    const result = await updateProjectSetup(id, request.setup, nextGeneration(), request.name, saved.revision);
    setBusy(false);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Confirming setup isn't in this build." : intakeCopy(result.code)); return; }
    const state = receiptState(result.value);
    if (state.kind === "rejected") { setProblem(intakeCopy(state.code)); return; }
    if (state.kind === "unconfirmed") { setProblem("We couldn't confirm setup was saved. Check the project and save again."); return; }
    void session.refreshProjects();
    session.showToast(`${draft.name.trim()} is set up.`);
    navigate({ name: "project", id, tab: "overview" });
  };

  return <OnboardingReviewView name={name} view={view} draft={draft} catalog={catalog} errors={errors} showErrors={showErrors} busy={busy} problem={problem}
    onDraft={setDraft} onConfirm={() => void confirm()} onOpenLink={(href) => void openLink(href)} onRetry={reloadView} />;
}
