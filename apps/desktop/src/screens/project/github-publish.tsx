import { useEffect, useState } from "react";
import type { Result } from "../../api";
import { approveGitHubPublish, continueGitHubPublish, discardGitHubPublish, excludeFromGitHubPublish, loadGitHubPublish, recheckGitHubPublish } from "../../api-intake";
import type { GitHubPublishJob } from "../../engine-client";
import { ConfirmDialog } from "../../ui";
import { displayText } from "../conversation";
import { DRAFT_POLL_MS, discardable, leftLine, publishView } from "./publish-job";

const ENGINE_STARTING = "CrazyAgent's engine is still starting. Try again in a moment.";

/** The Overview card for a project's GitHub publish (DEC-182): the draft while it runs, the
 * .gitignore lines to edit and approve, the secrets that block it with Leave this file out
 * and Check again, a stop with Try again, the finished repository, and Discard before the
 * push. Shows nothing while the project has no publish. Input: the project id. Output: the
 * card, or nothing. */
export function GitHubPublishCard({ projectId }: { projectId: string }) {
  const [read, setRead] = useState<Result<GitHubPublishJob | null> | null>(null);
  const [tick, setTick] = useState(0);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [edited, setEdited] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [left, setLeft] = useState("");
  const job = read?.kind === "ok" ? read.value : null;
  const view = publishView(job);

  useEffect(() => {
    let live = true;
    void loadGitHubPublish(projectId).then((result) => { if (live) setRead(result); });
    return () => { live = false; };
  }, [projectId, tick]);

  useEffect(() => {
    if (view.kind !== "drafting") return;
    const timer = setTimeout(() => setTick((value) => value + 1), DRAFT_POLL_MS);
    return () => clearTimeout(timer);
  }, [view.kind, read]);

  function failed(result: Exclude<Result<unknown>, { kind: "ok" }>) {
    setProblem(result.kind === "error" ? result.message : ENGINE_STARTING);
  }

  async function act(run: () => Promise<Result<GitHubPublishJob>>) {
    setBusy(true);
    setProblem(null);
    const result = await run();
    setBusy(false);
    if (result.kind !== "ok") return failed(result);
    setRead(result);
    setEdited(null);
  }

  async function discard() {
    setBusy(true);
    setProblem(null);
    const result = await discardGitHubPublish(projectId);
    setBusy(false);
    setConfirming(false);
    if (result.kind !== "ok") return failed(result);
    setLeft(leftLine(result.value));
    setRead({ kind: "ok", value: null });
    setEdited(null);
  }

  if (view.kind === "none" && !left && !problem) return null;
  const lines = view.kind === "lines" ? edited ?? view.lines.join("\n") : "";
  return <section className="pj-stack-s" aria-labelledby="pj-github-publish">
    <h2 id="pj-github-publish">Publish on GitHub</h2>
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
