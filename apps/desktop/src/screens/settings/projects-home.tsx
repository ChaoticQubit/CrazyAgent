import { useState } from "react";
import { nextGeneration, pickFolder } from "../../api";
import { intakeCopy, loadFolderSize, loadProjectsHome, moveProjectsHome, setProjectsHome } from "../../api-intake";
import { receiptState } from "../../api-projects";
import type { ProjectsHome } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, LoadError, NotReady, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { homeLine, pickedFolderProblem } from "../intake/intake";
import { formatBytes } from "../new-project/wizard";
import { folderLabel } from "../project/overview";
import "../project/projects.css";
import "../intake/intake.css";

const trimSlash = (path: string) => path.replace(/\/+$/u, "") || "/";

/** Checks a folder picked as the new projects folder. Inputs: the picked path and the current
 * projects folder or null. Output: a plain problem, or null when the folder can be used. */
export function homePickProblem(path: string, current: string | null): string | null {
  const issue = pickedFolderProblem(path, null);
  if (issue) return issue;
  if (current && trimSlash(path) === trimSlash(current)) return "This is already your projects folder.";
  if (current && trimSlash(path).startsWith(`${trimSlash(current)}/`)) return "Choose a folder outside your projects folder.";
  if (current && trimSlash(current).startsWith(`${trimSlash(path)}/`)) return "This folder holds your projects folder. Choose another one.";
  return null;
}

/** Says what Move my projects does before it runs, with the size read first.
 * Inputs: the current folder, the new folder and the current folder's size or null while it is
 * read. Output: the sentence. */
export function homeMoveLine(current: string, next: string, bytes: number | null): string {
  const from = folderLabel(current);
  const to = folderLabel(next);
  if (bytes === null) return `Checking how much is in ${from}...`;
  return `CrazyAgent moves every project folder in ${from} (${formatBytes(bytes)}) into ${to} and uses ${to} from then on. Agents can't run while it moves.`;
}

export type HomeAsk = { path: string; bytes: number | null; sizeProblem: string };

/** Settings, Projects folder as a view of its state: the folder in use, a picked folder with
 * Use this folder (moves nothing) and Move my projects there. Inputs: the home read, the picked
 * folder, a busy flag, the last problem and the handlers. Output: the page. */
export function ProjectsHomeSettingsView({ home, picked, busy, problem, onChoose, onUse, onMove, onCancel, onRetry }: {
  home: Loaded<ProjectsHome>;
  picked: string | null;
  busy: boolean;
  problem: string;
  onChoose: () => void;
  onUse: () => void;
  onMove: () => void;
  onCancel: () => void;
  onRetry: () => void;
}) {
  if (home.kind === "loading") return <Skeleton shape="line" count={3} />;
  if (home.kind === "not_ready") return <NotReady what="The projects folder" />;
  if (home.kind === "error") return <LoadError message={home.message} onRetry={onRetry} />;
  const current = home.value.path;
  const line = homeLine(home.value);
  return <div className="pj pj-stack">
    <p className="pj-secondary">CrazyAgent keeps each project in its own folder inside this one. Drop a project folder into it with Finder and it shows up on Home.</p>
    {current ? <p>Your projects folder is <span className="pj-mono" title={current}>{displayText(folderLabel(current))}</span>.</p>
      : <p>No projects folder yet. Choose one to start.</p>}
    {line && <p className="pj-small pj-muted">{line}</p>}
    {picked && <section className="pj-stack-s in-panel" aria-label="New projects folder">
      <p>Use <span className="pj-mono" title={picked}>{displayText(folderLabel(picked))}</span>?</p>
      {current && <p className="pj-secondary">{`Use this folder moves nothing. Projects already in ${displayText(folderLabel(current))} stay there. Move my projects there takes them with you.`}</p>}
      <span className="pj-row">
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={onUse}>Use this folder</Button>
        {current && <Button icon="folder" disabled={busy} onClick={onMove}>Move my projects there</Button>}
        <Button variant="quiet" disabled={busy} onClick={onCancel}>Cancel</Button>
      </span>
    </section>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    {!picked && <span><Button icon="folder-open" disabled={busy} onClick={onChoose}>{current ? "Choose another folder" : "Choose folder"}</Button></span>}
  </div>;
}

/** Settings, Projects folder: reads the folder, picks another in the macOS chooser, then either
 * switches to it without moving anything or, after showing the size, moves every project there. */
export function ProjectsHomeSettings() {
  const session = useAppSession();
  const [home, reload] = useLoad(loadProjectsHome);
  const [picked, setPicked] = useState<string | null>(null);
  const [ask, setAsk] = useState<HomeAsk | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const current = home.kind === "ok" ? home.value.path : null;

  const choose = async () => {
    setProblem("");
    const result = await pickFolder();
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "The folder chooser isn't in this build." : result.message); return; }
    if (result.value === null) return;
    const issue = homePickProblem(result.value, current);
    if (issue) setProblem(issue);
    else setPicked(result.value);
  };

  const change = async (send: () => ReturnType<typeof setProjectsHome>, what: string, done: string) => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const result = await send();
    setBusy(false);
    setAsk(null);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? `${what} isn't in this build.` : intakeCopy(result.code)); return; }
    const state = receiptState(result.value);
    if (state.kind === "rejected") { setProblem(intakeCopy(state.code)); return; }
    if (state.kind === "unconfirmed") { setProblem("We couldn't confirm the change. Check the folder shown here."); reload(); return; }
    setPicked(null);
    reload();
    void session.refreshProjects();
    session.showToast(done);
  };

  const askMove = async () => {
    if (!picked || !current || busy) return;
    const path = picked;
    setAsk({ path, bytes: null, sizeProblem: "" });
    const size = await loadFolderSize(current);
    setAsk((old) => old?.path !== path ? old
      : size.kind === "ok" ? { path, bytes: size.value.bytes, sizeProblem: "" }
      : { path, bytes: null, sizeProblem: size.kind === "not_ready" ? "Measuring the folder isn't in this build." : intakeCopy(size.code) });
  };

  return <>
    <ProjectsHomeSettingsView home={home} picked={picked} busy={busy} problem={problem} onRetry={reload}
      onChoose={() => void choose()} onCancel={() => { setPicked(null); setProblem(""); }} onMove={() => void askMove()}
      onUse={() => { if (picked) void change(() => setProjectsHome(picked, nextGeneration()), "Changing the projects folder", "Projects folder changed. Nothing was moved."); }} />
    <ConfirmDialog open={ask !== null && current !== null} title="Move your projects?" confirmLabel="Move projects" cancelLabel="Keep them here" pending={busy}
      onCancel={() => setAsk(null)} onConfirm={() => {
        if (ask && ask.bytes !== null) void change(() => moveProjectsHome(ask.path, nextGeneration()), "Moving projects", "Your projects are moving to the new folder.");
      }}>
      {ask && current ? (ask.sizeProblem ? `${ask.sizeProblem} Close this and try again.` : homeMoveLine(current, ask.path, ask.bytes)) : ""}
    </ConfirmDialog>
  </>;
}
