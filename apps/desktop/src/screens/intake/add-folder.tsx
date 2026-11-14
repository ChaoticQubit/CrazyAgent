import { useRef, useState } from "react";
import { pickFolder } from "../../api";
import { ADD_WARNINGS, loadFolderSize, loadIntakeJob, loadProjectsHome, startIntake } from "../../api-intake";
import type { AddFolderMode, AddOverride, FolderSize, IcloudProgress, ProjectsHome } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, Dialog, LoadError, Notice, NotReady, PageHeader, Segmented, Skeleton, formatRoute, navigate } from "../../ui";
import { displayText } from "../conversation";
import { folderLabel } from "../project/overview";
import { SetUpDialog } from "./found";
import {
  addWay, anywayIssues, anywayOverrides, anywayWarnings, folderName, icloudCountLine, icloudMoveLine,
  icloudPlaceholders, icloudProgressLine, joinPath, pickedFolderProblem, sizeLine, startReached, waysOffered, waysWithheld, worktreesLine, type AddWay, type AnywayIssue, type AnywayWarning,
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
          pendingLabel="Starting..." onClick={onAdd}>{ADD_LABELS[way]}</Button>
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
 * the folder name, its read, the engine refusal it follows, if any, the issues when the caller
 * already knows them (Home's answer to a job), a busy flag, the two answers and the close
 * handler. Output: the dialog, or nothing. */
export function AnywayDialog({ open, way, name, size, refused, issues, busy, onUseHere, onAnyway, onCancel }: {
  open: boolean;
  way: AddFolderMode;
  name: string;
  size: Loaded<FolderSize> | null;
  refused: string | null;
  issues?: AnywayIssue[];
  busy: boolean;
  onUseHere: () => void;
  onAnyway: () => void;
  onCancel?: () => void;
}) {
  const verb = way === "move" ? "Move" : "Copy";
  const blocks = anywayWarnings(way, issues ?? anywayIssues(way, size, refused), size);
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

/** The question before a Move or Copy of a folder with files only in iCloud: how many, that
 * CrazyAgent downloads them first, and that a move takes the folder out of iCloud on the owner's
 * other Macs; then Download and move, Download and copy or Cancel, the owner's way first in focus.
 * While the download runs it shows the progress and Stop download instead, and cannot be closed.
 * Inputs: whether it is open, the folder name and path, the count (null when only the engine's
 * refusal said some are), the way, the download state and the handlers. Output: the dialog. */
export function ICloudDialog({ open, name, source, placeholders, way, downloading, progress, stopping, onDownload, onStop, onCancel }: {
  open: boolean;
  name: string;
  source: string;
  placeholders: number | null;
  way: AddFolderMode;
  downloading: boolean;
  progress: IcloudProgress | null;
  stopping: boolean;
  onDownload: (way: AddFolderMode) => void;
  onStop: () => void;
  onCancel: () => void;
}) {
  const choice = (option: AddFolderMode, label: string) => <Button variant={option === way ? "primary" : "secondary"}
    icon={option === "move" ? "folder-input" : "copy"} onClick={() => onDownload(option)} data-autofocus={option === way || undefined}>{label}</Button>;
  const footer = downloading
    ? <Button pending={stopping} pendingLabel="Stopping..." onClick={onStop}>Stop download</Button>
    : <>{choice("move", "Download and move")}{choice("copy", "Download and copy")}<Button onClick={onCancel}>Cancel</Button></>;
  return <Dialog open={open} title={`Download ${name} from iCloud first?`} onClose={onCancel} busy={downloading} footer={footer}>
    <p>{icloudCountLine(placeholders)}</p>
    <Notice tone="info">{icloudMoveLine(source)}</Notice>
    {downloading && <p className="pj-secondary" role="status">{stopping ? "Stopping the download..." : icloudProgressLine(progress)}</p>}
  </Dialog>;
}

/** Add an existing folder: picks it in the macOS chooser and reads its size and git links. By
 * default it is set up where it is on a chosen pool; Move and Copy are offered once the read is
 * done. A Move or Copy with issues asks "Move anyway?" first: yes sends an override for exactly
 * those issues, Use where it is opens the setup in place. A Move with none asks to confirm with
 * the target shown. A folder with files only in iCloud asks before all of that whether to
 * download them and then move or copy. The move or copy then runs as the engine's intake job,
 * apart from this page: Add folder goes Home at once, where the job shows its progress, and
 * leaving or quitting never stops it. */
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
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [icloud, setIcloud] = useState<{ placeholders: number } | null>(null);
  const root = home.kind === "ok" ? home.value.path : null;
  const way = addWay(mode, size);

  const choose = async () => {
    setProblem("");
    setConfirming(false);
    setAsking(false);
    setIcloud(null);
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

  const add = async (overrides: AddOverride[], chosen: AddWay = way) => {
    if (!source || busy || chosen === "here") return;
    setBusy(true);
    setProblem("");
    const before = await loadIntakeJob();
    const result = await startIntake(source, chosen, overrides);
    const running = result.kind === "error" ? await loadIntakeJob() : null;
    setBusy(false);
    setConfirming(false);
    setAsking(false);
    if (result.kind === "not_ready") { setProblem("Adding a folder isn't in this build."); return; }
    const same = running?.kind === "ok" ? running.value : null;
    const attached = startReached(source, same, before);
    if (result.kind === "error" && !attached) { setProblem(result.message); return; }
    session.refreshIntake(result.kind === "ok" ? result.value : undefined);
    session.showToast(`Adding ${folderName(source)}. Follow it on Home; you can keep working.`, { name: "home" });
    navigate({ name: "home" });
  };

  const proceed = (chosen: AddFolderMode) => {
    if (anywayIssues(chosen, size, null).length > 0) setAsking(true);
    else if (chosen === "move") setConfirming(true);
    else void add([], chosen);
  };

  const start = () => {
    const placeholders = icloudPlaceholders(size);
    if (way === "here") setSettingUp(true);
    else if (placeholders > 0) setIcloud({ placeholders });
    else proceed(way);
  };

  const download = (chosen: AddFolderMode) => {
    setMode(chosen);
    setIcloud(null);
    proceed(chosen);
  };

  const anyway = () => {
    if (way === "here") return;
    void add(anywayOverrides(way, anywayIssues(way, size, null), size));
  };

  const useHere = () => {
    setAsking(false);
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
    {way !== "here" && <AnywayDialog open={asking} way={way} name={name} size={size} refused={null} busy={busy}
      onUseHere={useHere} onAnyway={anyway} onCancel={() => setAsking(false)} />}
    {way !== "here" && source && icloud && <ICloudDialog open name={name} source={source} placeholders={icloud.placeholders} way={way}
      downloading={false} progress={null} stopping={false} onDownload={download} onStop={() => setIcloud(null)}
      onCancel={() => setIcloud(null)} />}
  </>;
}
