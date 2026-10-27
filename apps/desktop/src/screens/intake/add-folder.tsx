import { useRef, useState } from "react";
import { nextGeneration, pickFolder } from "../../api";
import { addExistingFolder, intakeCopy, loadFolderSize, loadProjectsHome } from "../../api-intake";
import { receiptState } from "../../api-projects";
import type { FolderSize, ProjectsHome } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, LoadError, Notice, NotReady, PageHeader, Segmented, Skeleton, formatRoute, navigate } from "../../ui";
import { displayText } from "../conversation";
import { folderLabel } from "../project/overview";
import { SetUpDialog } from "./found";
import { folderName, joinPath, pickedFolderProblem, sizeLine, worktreesLine, type AddWay } from "./intake";
import "../project/projects.css";
import "./intake.css";

const WAYS: { value: AddWay; label: string; icon: "folder" | "folder-input" | "copy" }[] = [
  { value: "here", label: "Use where it is", icon: "folder" },
  { value: "move", label: "Move", icon: "folder-input" },
  { value: "copy", label: "Copy", icon: "copy" },
];

const ADD_LABELS: Record<AddWay, string> = { here: "Set up here", move: "Move into projects folder", copy: "Copy into projects folder" };

/** Add an existing folder as a view of its state: the chosen folder, used where it is (default)
 * or explicitly moved or copied with the target shown first, Copy with the size first and a
 * warning above 1 GB and the linked worktrees it leaves behind, and a clone suggestion when the
 * code is on GitHub. Inputs: the projects home
 * read, the source folder, its size read, the way, a busy flag, the last problem and the handlers.
 * Output: the page. */
export function AddFolderView({ home, source, size, mode, busy, problem, onChoose, onMode, onAdd, onClone, onRetry }: {
  home: Loaded<ProjectsHome>;
  source: string | null;
  size: Loaded<FolderSize> | null;
  mode: AddWay;
  busy: boolean;
  problem: string;
  onChoose: () => void;
  onMode: (mode: AddWay) => void;
  onAdd: () => void;
  onClone: () => void;
  onRetry?: () => void;
}) {
  const header = <PageHeader title="Add an existing folder" description="Set up a project you already have where it is, or move or copy it into your projects folder." />;
  if (home.kind === "loading") return <div className="pj in-page">{header}<Skeleton shape="line" count={4} /></div>;
  if (home.kind === "not_ready") return <div className="pj in-page">{header}<NotReady what="Adding an existing folder" /></div>;
  if (home.kind === "error") return <div className="pj in-page">{header}<LoadError message={home.message} onRetry={onRetry ?? onChoose} /></div>;
  const root = home.value.path;
  if (!root) return <div className="pj in-page">{header}
    <Notice tone="info">Choose your projects folder first. <a href={formatRoute({ name: "home" })}>Go to Home</a></Notice></div>;
  const bytes = size?.kind === "ok" ? size.value.bytes : null;
  const github = size?.kind === "ok" ? size.value.github ?? null : null;
  const worktrees = size?.kind === "ok" ? size.value.worktrees ?? 0 : 0;
  const line = sizeLine(mode, bytes, root);
  const name = source ? folderName(source) : "";
  return <div className="pj in-page">
    {header}
    <div className="pj-stack-s">
      <span className="pj-row"><Button icon="folder-open" disabled={busy} onClick={onChoose}>{source ? "Choose another folder" : "Choose folder"}</Button></span>
      {source && (mode === "here"
        ? <p><span className="pj-mono" title={source}>{displayText(folderLabel(source))}</span> stays where it is.</p>
        : <p><span className="pj-mono" title={source}>{displayText(folderLabel(source))}</span> goes to <span className="pj-mono">{displayText(folderLabel(joinPath(root, name)))}</span>.</p>)}
    </div>
    {source && <>
      {github && <Notice tone="info">
        This folder's code is on GitHub ({displayText(github)}). Cloning gives a fresh copy and leaves this folder alone.{" "}
        <Button size="small" icon="git-branch" onClick={onClone}>Clone instead</Button>
      </Notice>}
      <div className="pj-field">
        <span className="pj-label">How to add it</span>
        <Segmented label="How to add it" value={mode} onValue={onMode} options={WAYS} />
      </div>
      {line.warn ? <Notice tone="warn">{line.text}</Notice> : <p className="pj-secondary" role="status">{line.text}</p>}
      {mode === "copy" && worktrees > 0 && <Notice tone="info">{worktreesLine(worktrees)}</Notice>}
      {size?.kind === "not_ready" && mode === "copy" && <p className="pj-help">The folder size isn't in this build, so Copy can't show it first.</p>}
      {size?.kind === "error" && <p className="pj-error" role="alert">{size.message}</p>}
      {problem && <p className="pj-error" role="alert">{problem}</p>}
      <span className="pj-row">
        <Button variant="primary" icon={mode === "here" ? "list-checks" : mode === "move" ? "folder-input" : "copy"} pending={busy}
          pendingLabel={mode === "move" ? "Moving..." : "Copying..."} disabled={mode === "copy" && bytes === null}
          onClick={onAdd}>{ADD_LABELS[mode]}</Button>
        <a className="btn quiet" href={formatRoute({ name: "home" })}>Cancel</a>
      </span>
    </>}
    {!source && problem && <p className="pj-error" role="alert">{problem}</p>}
  </div>;
}

/** Add an existing folder: picks it in the macOS chooser and reads its size. By default it is set up
 * where it is on a chosen pool; Move asks to confirm with the target shown, and a moved or copied
 * folder shows on Home as found. */
export function AddFolder() {
  const session = useAppSession();
  const [home, reload] = useLoad(loadProjectsHome, [], "projects-home");
  const picks = useRef(0);
  const [source, setSource] = useState<string | null>(null);
  const [size, setSize] = useState<Loaded<FolderSize> | null>(null);
  const [mode, setMode] = useState<AddWay>("here");
  const [settingUp, setSettingUp] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const root = home.kind === "ok" ? home.value.path : null;

  const choose = async () => {
    setProblem("");
    const result = await pickFolder();
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "The folder chooser isn't in this build." : result.message); return; }
    if (result.value === null) return;
    const issue = pickedFolderProblem(result.value, root);
    if (issue) { setProblem(issue); return; }
    const path = result.value;
    const pick = picks.current + 1;
    picks.current = pick;
    setSource(path);
    setSize({ kind: "loading" });
    const measured = await loadFolderSize(path);
    if (picks.current === pick) setSize(measured);
  };

  const add = async () => {
    if (!source || busy || mode === "here") return;
    setBusy(true);
    setProblem("");
    const result = await addExistingFolder(source, mode, nextGeneration());
    setBusy(false);
    setConfirming(false);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Adding a folder isn't in this build." : intakeCopy(result)); return; }
    const state = receiptState(result.value);
    if (state.kind === "rejected") { setProblem(intakeCopy(state)); return; }
    if (state.kind === "unconfirmed") { setProblem("We couldn't confirm the folder was added. Check Home before trying again."); return; }
    session.showToast(`${folderName(source)} is in your projects folder. Set it up from Home.`);
    navigate({ name: "home" });
  };

  const start = () => {
    if (mode === "here") setSettingUp(true);
    else if (mode === "move") setConfirming(true);
    else void add();
  };

  const name = source ? folderName(source) : "";
  return <>
    <AddFolderView home={home} source={source} size={size} mode={mode} busy={busy} problem={problem}
      onChoose={() => void choose()} onMode={setMode} onAdd={start} onClone={() => navigate({ name: "add-project", way: "clone" })} onRetry={reload} />
    {settingUp && source && <SetUpDialog folder={{ name, path: source }} onClose={() => setSettingUp(false)}
      onStarted={() => { setSettingUp(false); session.showToast(`Setting up ${name}. Continue setup from Home.`); navigate({ name: "home" }); }} />}
    <ConfirmDialog open={confirming && source !== null && root !== null} title={`Move ${name} into your projects folder?`} confirmLabel="Move folder" cancelLabel="Keep it here" pending={busy}
      onCancel={() => setConfirming(false)} onConfirm={() => void add()}>
      {displayText(folderLabel(source ?? ""))} moves to {displayText(folderLabel(root ? joinPath(root, name) : ""))} and won't stay at its old place.
      Anything that points at the old place, like a terminal or an editor, has to open the new one.
    </ConfirmDialog>
  </>;
}
