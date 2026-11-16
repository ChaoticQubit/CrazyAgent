import { useEffect, useState } from "react";
import type { Result } from "../../api";
import { answerGitHubPublishPrompt, approveGitHubPublish, continueGitHubPublish, discardGitHubPublish, excludeFromGitHubPublish, loadGitHubPublish, loadGitHubPublishPrompt, recheckGitHubPublish } from "../../api-intake";
import type { GitHubPublishJob } from "../../engine-client";
import { ConfirmDialog } from "../../ui";
import { displayText } from "../conversation";
import { PublishFields, useGitHubPublish } from "../new-project/fields";
import { createWait, emptyPublish, overviewWait, publishOutcome, startPublish, startReplyLost } from "../new-project/publish";
import { emptyDraft } from "../new-project/wizard";
import type { ProjectDraft } from "../new-project/wizard";
import { DRAFT_POLL_MS, ENGINE_STARTING, PROMPT_LINE, PROMPT_NEVER, PROMPT_NOT_NOW, PROMPT_PUBLISH, PUBLISH_NOT_SEEN, discardable, forgetLostStart, leftLine, lostStartSince, lostStartStep, noteLostStart, noticeShown, publishView, readKept } from "./publish-job";

/** The New Project publish fields for a project made without them, opened by the Overview's
 * Publish: the visibility, account, repository name, description and docs switch, each name
 * and description following the project's until edited. Inputs: the project's id, name and
 * about, what to do with the Start's job, its line and whether its reply was lost, and
 * Cancel. Output: the form. */
function PublishForm({ projectId, name, about, onStarted, onCancel }: { projectId: string; name: string; about: string; onStarted: (job: GitHubPublishJob | null, line: string, lost: boolean) => void; onCancel: () => void }) {
  const github = useGitHubPublish();
  const [draft, setDraft] = useState<ProjectDraft>(() => ({ ...emptyDraft(), name, about, publish: emptyPublish() }));
  const [busy, setBusy] = useState(false);
  const publish = draft.publish ?? emptyPublish();
  const wait = createWait(publish, name, about, github.offered, github.signedIn, github.checking);
  const why = overviewWait(wait);

  async function start() {
    const started = startPublish(projectId, publish, name, about, github.offered);
    if (!started) return;
    setBusy(true);
    const result = await started;
    setBusy(false);
    onStarted(result.kind === "ok" ? result.value : null, publishOutcome(result), startReplyLost(result));
  }

  return <div className="pj-stack-s">
    <PublishFields prefix="pj-github" draft={draft} onChange={setDraft} github={github} place="overview" />
    {why && <p className="pj-muted" id="pj-github-why">{why}</p>}
    <div className="pj-need">
      <button type="button" className="btn primary" disabled={busy || !publish.on || wait !== null} aria-describedby={why ? "pj-github-why" : undefined} onClick={() => void start()}>Publish on GitHub</button>
      <button type="button" className="btn" disabled={busy} onClick={onCancel}>Cancel</button>
    </div>
  </div>;
}

/** The Overview card for a project's GitHub publish (DEC-182): the draft while it runs, the
 * .gitignore lines to edit and approve, the secrets that block it with Leave this file out
 * and Check again, a stop with Try again, the finished repository, and Discard before the
 * push. A failed read keeps the last job with the problem and Try again, and a failed answer
 * reads the job again. With no publish it asks "This project is not on GitHub. Publish it?" when the engine
 * says to, with Publish, Not now and Don't ask again. After a Start whose reply was lost,
 * here or in Create, it reads the publish every two seconds with no question until a
 * publish shows, or says none showed once the Start's budget passed and a read found none,
 * also when opened again (GSB-1, GSB2). Inputs: the project's id, name and about. Output:
 * the card, or nothing. */
export function GitHubPublishCard({ projectId, name, about }: { projectId: string; name: string; about: string }) {
  const [read, setRead] = useState<Result<GitHubPublishJob | null> | null>(null);
  const [tick, setTick] = useState(0);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [edited, setEdited] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [left, setLeft] = useState("");
  const [prompt, setPrompt] = useState<Result<boolean> | null>(null);
  const [form, setForm] = useState(false);
  const [said, setSaid] = useState("");
  const [readFailed, setReadFailed] = useState<string | null>(null);
  const [answered, setAnswered] = useState(0);
  const [lostSince, setLostSince] = useState<number | null>(() => lostStartSince(projectId));
  const job = read?.kind === "ok" ? read.value : null;
  const view = publishView(job);
  const noJob = read?.kind === "ok" && read.value === null;
  const notice = noticeShown(read, prompt);

  useEffect(() => {
    let live = true;
    void loadGitHubPublish(projectId).then((result) => {
      if (!live) return;
      setReadFailed(readKept(null, result).failed);
      setRead((kept) => readKept(kept, result).read);
      setAnswered((count) => count + 1);
    });
    return () => { live = false; };
  }, [projectId, tick]);

  useEffect(() => {
    if (!noJob || lostSince !== null) return;
    let live = true;
    void loadGitHubPublishPrompt(projectId).then((result) => { if (live) setPrompt(result); });
    return () => { live = false; };
  }, [projectId, noJob, lostSince]);

  useEffect(() => {
    const step = lostStartStep(lostSince, answered, read, Date.now());
    if (step === "idle") return;
    if (step === "read") {
      const timer = setTimeout(() => setTick((value) => value + 1), DRAFT_POLL_MS);
      return () => clearTimeout(timer);
    }
    forgetLostStart(projectId);
    setLostSince(null);
    if (step === "over") setProblem(PUBLISH_NOT_SEEN);
  }, [lostSince, answered, read]);

  useEffect(() => {
    if (view.kind !== "drafting") return;
    const timer = setTimeout(() => setTick((value) => value + 1), DRAFT_POLL_MS);
    return () => clearTimeout(timer);
  }, [view.kind, answered]);

  function failed(result: Exclude<Result<unknown>, { kind: "ok" }>) {
    setProblem(result.kind === "error" ? result.message : ENGINE_STARTING);
  }

  async function act(run: () => Promise<Result<GitHubPublishJob>>) {
    setBusy(true);
    setProblem(null);
    const result = await run();
    setBusy(false);
    if (result.kind !== "ok") {
      failed(result);
      setTick((value) => value + 1);
      return;
    }
    setRead(result);
    setEdited(null);
  }

  async function answer(run: () => Promise<Result<null>>) {
    setBusy(true);
    setProblem(null);
    const result = await run();
    setBusy(false);
    if (result.kind !== "ok") return failed(result);
    setPrompt({ kind: "ok", value: false });
  }

  function started(made: GitHubPublishJob | null, line: string, lost: boolean) {
    if (!made) {
      setProblem(line);
      setTick((value) => value + 1);
      if (lost) {
        const at = Date.now();
        noteLostStart(projectId, at);
        setLostSince(at);
      }
      return;
    }
    setForm(false);
    setProblem(null);
    setSaid(line);
    setRead({ kind: "ok", value: made });
  }

  async function discard() {
    setBusy(true);
    setProblem(null);
    const result = await discardGitHubPublish(projectId);
    setBusy(false);
    setConfirming(false);
    if (result.kind !== "ok") {
      failed(result);
      setTick((value) => value + 1);
      return;
    }
    setLeft(leftLine(result.value));
    setRead({ kind: "ok", value: null });
    setEdited(null);
  }

  if (view.kind === "none" && !left && !problem && !notice && !form && !readFailed) return null;
  const lines = view.kind === "lines" ? edited ?? view.lines.join("\n") : "";
  return <section className="pj-stack-s" aria-labelledby="pj-github-publish">
    <h2 id="pj-github-publish">Publish on GitHub</h2>
    {readFailed && <div className="pj-need"><span className="pj-error" role="alert">{displayText(readFailed)}</span>
      <button type="button" className="btn" id="pj-github-reread" onClick={() => setTick((value) => value + 1)}>Try again</button></div>}
    {notice && !form && <div className="pj-need"><span>{PROMPT_LINE}</span>
      <button type="button" className="btn primary" disabled={busy} onClick={() => setForm(true)}>{PROMPT_PUBLISH}</button>
      <button type="button" className="btn" disabled={busy} onClick={() => void answer(() => answerGitHubPublishPrompt(projectId, "not_now"))}>{PROMPT_NOT_NOW}</button>
      <button type="button" className="btn" disabled={busy} onClick={() => void answer(() => answerGitHubPublishPrompt(projectId, "never"))}>{PROMPT_NEVER}</button></div>}
    {form && view.kind === "none" && <PublishForm projectId={projectId} name={name} about={about} onStarted={started} onCancel={() => setForm(false)} />}
    {said && <p className="pj-muted" role="status">{displayText(said)}</p>}
    {view.kind === "drafting" && <p className="pj-muted" role="status">Drafting the .gitignore lines...</p>}
    {view.kind === "lines" && <>
      <p className="pj-help">{view.note}</p>
      <label className="pj-label" htmlFor="pj-github-lines">.gitignore lines, one per line</label>
      <textarea id="pj-github-lines" className="pj-mono" rows={Math.min(12, Math.max(4, lines.split("\n").length))} value={lines} disabled={busy}
        onChange={(event) => setEdited(event.target.value)} />
      <div className="pj-need"><button type="button" className="btn primary" disabled={busy}
        onClick={() => void act(() => approveGitHubPublish(projectId, lines.split("\n").map((line) => line.trim()).filter(Boolean)))}>Approve and publish</button></div>
    </>}
    {view.kind === "blocked" && <>
      <p>These files look like they hold secrets, so nothing was published.</p>
      <ul className="pj-mono">{view.findings.map((finding) => <li key={finding}>{displayText(finding)}</li>)}</ul>
      {view.truncated && <p className="pj-muted">There are more. Check again after these.</p>}
      {view.paths.map((path) => <div className="pj-need" key={path}><span className="pj-mono">{displayText(path)}</span>
        <button type="button" className="btn" disabled={busy} onClick={() => void act(() => excludeFromGitHubPublish(projectId, path))}>Leave this file out</button></div>)}
      <div className="pj-need"><button type="button" className="btn" disabled={busy} onClick={() => void act(() => recheckGitHubPublish(projectId))}>Check again</button></div>
    </>}
    {view.kind === "stopped" && <div className="pj-need"><span>{displayText(view.reason)}</span>
      <button type="button" className="btn primary" disabled={busy} onClick={() => void act(() => continueGitHubPublish(projectId))}>Try again</button></div>}
    {view.kind === "done" && <>
      <p>{displayText(view.line)}</p>
      {view.missed.length > 0 && <p className="pj-muted">Not set: {view.missed.join(", ")}.</p>}
    </>}
    {left && <p className="pj-muted">{displayText(left)}</p>}
    {problem && <p className="pj-error" role="alert">{displayText(problem)}</p>}
    {discardable(job) && <div className="pj-need"><button type="button" className="btn" disabled={busy} onClick={() => setConfirming(true)}>Discard</button></div>}
    <ConfirmDialog open={confirming} title="Discard this publish?" confirmLabel="Discard" pending={busy}
      onConfirm={() => void discard()} onCancel={() => setConfirming(false)}>
      CrazyAgent forgets this publish so you can start again. Files it wrote in the project stay, and a repository it made stays on GitHub.
    </ConfirmDialog>
  </section>;
}
