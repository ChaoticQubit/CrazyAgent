import { useRef, useState } from "react";
import { nextGeneration, pickFolder } from "../../api";
import { ADD_WARNINGS, addExistingFolder, intakeCopy, loadFolderSize, loadProjectsHome } from "../../api-intake";
import { receiptState } from "../../api-projects";
import type { AddFolderMode, AddOverride, FolderSize, ProjectsHome } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, Dialog, LoadError, Notice, NotReady, PageHeader, Segmented, Skeleton, formatRoute, navigate } from "../../ui";
import { displayText } from "../conversation";
import { folderLabel } from "../project/overview";
import { SetUpDialog } from "./found";
import {
  addWay, anywayIssues, anywayOverrides, asksAgain, anywayWarnings, folderName, joinPath, pickedFolderProblem, sizeLine, waysOffered, waysWithheld,
  worktreesLine, type AddWay, type AnywayWarning,
} from "./intake";
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
 * warning above 1 GB or when it could not be measured and the linked worktrees it leaves behind,
 * Move and Copy once the folder read has finished (what it found is asked about before anything
 * moves), and a clone suggestion when the code is on GitHub. Inputs: the projects home read, the
 * source folder, its size read, the way, a busy flag, the last problem and the handlers.
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
  const offered = waysOffered(size);
  const withheld = waysWithheld(size);
  const way = addWay(mode, size);
  const line = sizeLine(way, bytes, root, offered && bytes === null);
  const name = source ? folderName(source) : "";
  return <div className="pj in-page">
    {header}
    <div className="pj-stack-s">
      <span className="pj-row"><Button icon="folder-open" disabled={busy} onClick={onChoose}>{source ? "Choose another folder" : "Choose folder"}</Button></span>
      {source && (way === "here"
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
        <Segmented label="How to add it" value={way} onValue={onMode} options={WAYS.filter((option) => offered || option.value === "here")} />
      </div>
      {withheld.map((line) => <Notice key={line} tone="info">{line}</Notice>)}
      {line.warn ? <Notice tone="warn">{line.text}</Notice> : <p className="pj-secondary" role="status">{line.text}</p>}
      {way === "copy" && worktrees > 0 && <Notice tone="info">{worktreesLine(worktrees)}</Notice>}
      {size?.kind === "error" && <p className="pj-error" role="alert">{size.message}</p>}
      {problem && <p className="pj-error" role="alert">{problem}</p>}
      <span className="pj-row">
        <Button variant="primary" icon={way === "here" ? "list-checks" : way === "move" ? "folder-input" : "copy"} pending={busy}
          pendingLabel={way === "move" ? "Moving..." : "Copying..."} onClick={onAdd}>{ADD_LABELS[way]}</Button>
        <a className="btn quiet" href={formatRoute({ name: "home" })}>Cancel</a>
      </span>
    </>}
    {!source && problem && <p className="pj-error" role="alert">{problem}</p>}
  </div>;
}

/** The warning blocks of a Move or Copy question: data loss blocks as warnings, other effects as
 * information, each with the folders it names. Input: the blocks in order. Output: the list. */
function AddWarnings({ blocks }: { blocks: AnywayWarning[] }) {
  return <div className="pj-stack-s">
    {blocks.map((block) => <Notice key={block.lines.join(" ")} tone={block.loss ? "warn" : "info"}>
      {block.lines.map((line) => <p key={line}>{line}</p>)}
      {block.paths.length > 0 && <ul>{block.paths.map((path) => <li key={path} className="pj-mono">{displayText(path)}</li>)}</ul>}
    </Notice>)}
  </div>;
}

/** The question before a Move or Copy of a folder with issues: what each issue means, data loss
 * first, then "Move anyway?" or "Copy anyway?". It starts on Use where it is; Move anyway or Copy
 * anyway is the danger button; closing it changes nothing. Inputs: whether it is open, the way,
 * the folder name, its read, the engine refusal it follows, if any, a busy flag, the two answers
 * and the close handler. Output: the dialog, or nothing. */
export function AnywayDialog({ open, way, name, size, refused, busy, onUseHere, onAnyway, onCancel }: {
  open: boolean;
  way: AddFolderMode;
  name: string;
  size: Loaded<FolderSize> | null;
  refused: string | null;
  busy: boolean;
  onUseHere: () => void;
  onAnyway: () => void;
  onCancel?: () => void;
}) {
  const verb = way === "move" ? "Move" : "Copy";
  const blocks = anywayWarnings(way, anywayIssues(way, size, refused), size);
  return <Dialog open={open} title={`${verb} ${name} anyway?`} onClose={onCancel ?? onUseHere} busy={busy}
    footer={<><Button disabled={busy} onClick={onUseHere} data-autofocus>Use where it is</Button>
      <Button variant="danger" pending={busy} onClick={onAnyway}>{`${verb} anyway`}</Button></>}>
    <AddWarnings blocks={blocks} />
  </Dialog>;
}

/** The confirm before a Move with no issues: the folder, where it goes and what a move can lose,
 * apps still using it first, then the old place and whether it is a quick rename or a copy across
 * disks. It shows only while asked for and while Move is still the way. Inputs: whether it is
 * asked for, the way, the folder name, source and projects folder, whether both are on one disk,
 * a busy flag and the handlers. Output: the dialog, or nothing. */
export function MoveConfirmDialog({ open, way, name, source, root, sameVolume, busy, onCancel, onConfirm }: {
  open: boolean;
  way: AddWay;
  name: string;
  source: string | null;
  root: string | null;
  sameVolume: boolean | null;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const loss = [ADD_WARNINGS.close_apps, ...(sameVolume === false ? [ADD_WARNINGS.across_disks] : [])];
  const effects = [ADD_WARNINGS.old_place, ...(sameVolume === true ? [ADD_WARNINGS.same_disk] : [])];
  return <Dialog open={open && way === "move" && source !== null && root !== null} title={`Move ${name} into your projects folder?`} onClose={onCancel} busy={busy}
    footer={<><Button disabled={busy} onClick={onCancel} data-autofocus>Keep it here</Button>
      <Button variant="danger" pending={busy} onClick={onConfirm}>Move folder</Button></>}>
    <p>{displayText(folderLabel(source ?? ""))} moves to {displayText(folderLabel(root ? joinPath(root, name) : ""))}.</p>
    <AddWarnings blocks={[{ loss: true, lines: loss, paths: [] }, { loss: false, lines: effects, paths: [] }]} />
  </Dialog>;
}

/** Add an existing folder: picks it in the macOS chooser and reads its size and git links. By
 * default it is set up where it is on a chosen pool; Move and Copy are offered once the read is
 * done. A Move or Copy with issues asks "Move anyway?" first: yes sends an override for exactly
 * those issues, Use where it is opens the setup in place. A Move with none asks to confirm with
 * the target shown, and a moved or copied folder shows on Home as found. */
export function AddFolder() {
  const session = useAppSession();
  const [home, reload] = useLoad(loadProjectsHome, [], "projects-home");
  const picks = useRef(0);
  const [source, setSource] = useState<string | null>(null);
  const [size, setSize] = useState<Loaded<FolderSize> | null>(null);
  const [mode, setMode] = useState<AddWay>("here");
  const [settingUp, setSettingUp] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [asking, setAsking] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const root = home.kind === "ok" ? home.value.path : null;
  const way = addWay(mode, size);

  const choose = async () => {
    setProblem("");
    setConfirming(false);
    setAsking(false);
    setRefused(null);
    const result = await pickFolder("project");
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

  const add = async (overrides: AddOverride[]) => {
    if (!source || busy || way === "here") return;
    setBusy(true);
    setProblem("");
    const result = await addExistingFolder(source, way, overrides, nextGeneration());
    setBusy(false);
    setConfirming(false);
    setAsking(false);
    setRefused(null);
    if (result.kind === "not_ready") { setProblem("Adding a folder isn't in this build."); return; }
    const state = result.kind === "ok" ? receiptState(result.value) : null;
    const code = result.kind === "error" ? result.code : state?.kind === "rejected" ? state.code : null;
    if (code !== null && asksAgain(code, overrides)) { setRefused(code); setAsking(true); return; }
    if (result.kind === "error") { setProblem(intakeCopy(result)); return; }
    if (state?.kind === "rejected") { setProblem(intakeCopy(state)); return; }
    if (state?.kind === "unconfirmed") { setProblem("We couldn't confirm the folder was added. Check Home before trying again."); return; }
    session.showToast(`${folderName(source)} is in your projects folder. Set it up from Home.`);
    navigate({ name: "home" });
  };

  const start = () => {
    if (way === "here") setSettingUp(true);
    else if (anywayIssues(way, size, null).length > 0) setAsking(true);
    else if (way === "move") setConfirming(true);
    else void add([]);
  };

  const anyway = () => {
    if (way === "here") return;
    void add(anywayOverrides(way, anywayIssues(way, size, refused), size));
  };

  const useHere = () => {
    setAsking(false);
    setRefused(null);
    setMode("here");
    setSettingUp(true);
  };

  const name = source ? folderName(source) : "";
  return <>
    <AddFolderView home={home} source={source} size={size} mode={mode} busy={busy} problem={problem}
      onChoose={() => void choose()} onMode={setMode} onAdd={start} onClone={() => navigate({ name: "add-project", way: "clone" })} onRetry={reload} />
    {settingUp && source && <SetUpDialog folder={{ name, path: source }} onClose={() => setSettingUp(false)}
      onStarted={() => { setSettingUp(false); session.showToast(`Setting up ${name}. Continue setup from Home.`); navigate({ name: "home" }); }} />}
    <MoveConfirmDialog open={confirming} way={way} name={name} source={source} root={root}
      sameVolume={size?.kind === "ok" ? size.value.same_volume ?? null : null} busy={busy}
      onCancel={() => setConfirming(false)} onConfirm={() => void add([])} />
    {way !== "here" && <AnywayDialog open={asking} way={way} name={name} size={size} refused={refused} busy={busy}
      onUseHere={useHere} onAnyway={anyway} onCancel={() => { setAsking(false); setRefused(null); }} />}
  </>;
}
