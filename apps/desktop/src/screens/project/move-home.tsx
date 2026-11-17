import { useContext, useEffect, useRef, useState } from "react";
import { setTeamStopped } from "../../api";
import type { Result } from "../../api";
import { ADD_WARNINGS, loadFolderSize, loadProjectsHome, moveCopy } from "../../api-intake";
import { moveProjectHome } from "../../api-projects";
import type { StoredSetup } from "../../api-projects";
import type { AddFolderMode, AddOverride, FolderSize, IntakeJob, IntentReceipt } from "../../engine-client";
import { SessionContext, useLoad, type Loaded } from "../../state";
import { Button, Dialog, Notice, Segmented } from "../../ui";
import { displayText } from "../conversation";
import { AnywayDialog, leftInIcloud } from "../intake/add-folder";
import { anywayIssues, anywayOverrides, asksAgain, folderName, joinPath, jobRunning, receiptRefusal, sizeLine, withEverything, worktreesLine } from "../intake/intake";
import { folderLabel, projectFolder } from "./overview";
import type { ProjectData } from "./shared";

export { moveCopy };

const NO_JOBS: readonly IntakeJob[] = [];

const WAYS: { value: AddFolderMode; label: string; icon: "folder-input" | "copy" }[] = [
  { value: "move", label: "Move", icon: "folder-input" },
  { value: "copy", label: "Copy", icon: "copy" },
];

/** Whether a project folder is the projects folder or inside it. Inputs: the project folder and
 * the projects folder, null when the owner has not picked one. Output: the answer. */
export function insideHome(folder: string, home: string | null): boolean {
  return home !== null && (folder === home || folder.startsWith(`${home}/`));
}

/** The settings row that offers moving a project kept where it is into the projects folder.
 * It shows nothing for a project already inside that folder or before the owner picked one.
 * Inputs: the project folder, the projects folder and the open handler. Output: the row. */
export function MoveHomeRow({ folder, home, onOpen }: { folder: string; home: string | null; onOpen: () => void }) {
  if (home === null || insideHome(folder, home)) return null;
  return <div className="pj-stack">
    <h2>Move into CrazyAgent folder</h2>
    <p>This project is kept at <span className="pj-mono">{displayText(folderLabel(folder))}</span>, outside your projects folder. Move or copy it to <span className="pj-mono">{displayText(folderLabel(joinPath(home, folderName(folder))))}</span>.</p>
    <div><Button icon="folder-input" onClick={onOpen}>Move into CrazyAgent folder</Button></div>
  </div>;
}

export interface MoveSteps {
  stop: (id: string, stopped: boolean) => Promise<Result<IntentReceipt>>;
  move: (id: string, mode: AddFolderMode, overrides: AddOverride[]) => Promise<Result<IntakeJob>>;
}

/** Stops the project's team, then starts the engine's move job, only once the stop was applied
 * (DEC-132 Q11). Inputs: the project id, move or copy, the soft refusals the owner chose to go
 * past and the two engine calls. Output: whether the team was stopped and the result of the step
 * that ended the run: the stop's failure or refusal, else the started job. */
export async function stopThenMove(id: string, mode: AddFolderMode, overrides: AddOverride[], steps: MoveSteps): Promise<{ stopped: boolean; result: Result<IntakeJob> }> {
  const stop = await steps.stop(id, true);
  if (stop.kind !== "ok") return { stopped: false, result: stop };
  const refusal = receiptRefusal(stop.value);
  if (refusal !== null) return { stopped: false, result: { kind: "error", code: refusal, message: moveCopy(refusal) } };
  return { stopped: true, result: await steps.move(id, mode, overrides) };
}

/** Whether a move's folder read ended without a reading: an error, a build without the read, or
 * an ok reply carrying the engine's size problem (timeout, too large, unreadable), which the
 * bridge sends with no bytes. Input: the read. Output: the answer, false while it loads or before
 * it starts. */
function readFailed(size: Loaded<FolderSize> | null): boolean {
  if (size === null || size.kind === "loading") return false;
  return size.kind !== "ok" || size.value.size_problem != null;
}

/** The overrides a project move sends: the owner's, plus download_everything when the switch is
 * on or the folder read failed, as a move never leaves folders in iCloud it did not show first
 * (DEC-198, leader 16:30). Inputs: the overrides, the switch and the folder read. Output: the
 * overrides to send. */
export function moveOverrides(overrides: AddOverride[], everything: boolean, size: Loaded<FolderSize> | null): AddOverride[] {
  return withEverything(overrides, everything || readFailed(size));
}

/** The question before a project moves: what happens to the team and its work, Move or Copy,
 * where the folder goes, its size, the rebuildable folders it leaves in iCloud with the Download
 * everything (slow) switch, and what a move or copy can lose. Inputs: whether it is open, the
 * project name, its folder, the projects folder, the way, the folder read, a busy flag, the last
 * problem, the switch and the handlers. Output: the dialog, or nothing. */
export function MoveHomeDialog({ open, name, folder, home, way, size, busy, problem, everything, onEverything, onWay, onConfirm, onCancel }: {
  open: boolean;
  name: string;
  folder: string;
  home: string;
  way: AddFolderMode;
  size: Loaded<FolderSize> | null;
  busy: boolean;
  problem: string;
  everything: boolean;
  onEverything: (everything: boolean) => void;
  onWay: (way: AddFolderMode) => void;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const read = size?.kind === "ok" ? size.value : null;
  const ready = size !== null && size.kind !== "loading";
  const line = sizeLine(way, read?.bytes ?? null, home, ready && (read?.bytes ?? null) === null);
  const loss = way === "move"
    ? [ADD_WARNINGS.close_apps, ...(read?.same_volume === false ? [ADD_WARNINGS.across_disks] : [])]
    : [ADD_WARNINGS.copy_all];
  const effects = way === "move" ? [ADD_WARNINGS.old_place, ...(read?.same_volume === true ? [ADD_WARNINGS.same_disk] : [])] : [];
  return <Dialog open={open} title={`Move ${name} into your CrazyAgent folder?`} onClose={onCancel} busy={busy}
    footer={<><Button disabled={busy} onClick={onCancel} data-autofocus>Keep it here</Button>
      <Button variant="danger" pending={busy} pendingLabel={way === "move" ? "Moving..." : "Copying..."} disabled={!ready} onClick={onConfirm}>
        {way === "move" ? "Stop the team and move" : "Stop the team and copy"}</Button></>}>
    <p>Move {name} into your CrazyAgent folder. The team stops first; work not landed yet stays in the agents' copies, which follow the move.</p>
    <Segmented label="Move or copy" value={way} onValue={onWay} options={WAYS} />
    <p><span className="pj-mono">{displayText(folderLabel(folder))}</span> {way === "move" ? "moves" : "is copied"} to <span className="pj-mono">{displayText(folderLabel(joinPath(home, folderName(folder))))}</span>.</p>
    {!ready && <p className="pj-secondary" role="status">Reading the folder...</p>}
    {ready && (line.warn ? <Notice tone="warn">{line.text}</Notice> : <p className="pj-secondary" role="status">{line.text}</p>)}
    {leftInIcloud({ skipped: read?.skipped ?? [], skippedMore: read?.skipped_more ?? null, everything, onEverything, source: folder, job: way })}
    {readFailed(size) && <Notice tone="info">CrazyAgent couldn't read this folder. Anything in it only in iCloud is downloaded first, rebuildable folders too, which can be slow.</Notice>}
    {way === "copy" && <Notice tone="info">The copy becomes the project. The old folder stays where it is, with the agents' old copies in it.</Notice>}
    {way === "copy" && (read?.worktrees ?? 0) > 0 && <Notice tone="info">{worktreesLine(read?.worktrees ?? 0)}</Notice>}
    <Notice tone="warn">{loss.map((text) => <p key={text}>{text}</p>)}</Notice>
    {effects.length > 0 && <Notice tone="info">{effects.map((text) => <p key={text}>{text}</p>)}</Notice>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
  </Dialog>;
}

/** Move into CrazyAgent folder, for a project kept where the owner made it: the settings row,
 * the Move or Copy question and the add-folder "anyway" question for a folder with issues.
 * Confirming stops the team, then starts the engine's move job, which the owner follows on Home
 * while they keep working: it downloads what is only in iCloud, waits for the agents, moves the
 * folder and points the project and its agents' copies at the new place (DEC-177). The page
 * reads the project again once that job ended. Inputs: the project data and its saved setup.
 * Output: the row with its dialogs, or nothing for a project already in the projects folder. */
export function MoveHome({ data, setup }: { data: Pick<ProjectData, "id" | "name" | "notify" | "reload">; setup: StoredSetup }) {
  const session = useContext(SessionContext);
  const [home] = useLoad(loadProjectsHome, [], "projects-home");
  const folder = projectFolder(setup.folder);
  const root = home.kind === "ok" ? home.value.path : null;
  const [open, setOpen] = useState(false);
  const [way, setWay] = useState<AddFolderMode>("move");
  const [everything, setEverything] = useState(false);
  const [size, setSize] = useState<Loaded<FolderSize> | null>(null);
  const [asking, setAsking] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [started, setStarted] = useState<string | null>(null);
  const seen = useRef(false);
  const jobs = session?.intake ?? NO_JOBS;
  useEffect(() => {
    if (!open) return;
    let live = true;
    setSize({ kind: "loading" });
    void loadFolderSize(folder).then((read) => { if (live) setSize(read); });
    return () => { live = false; };
  }, [open, folder]);
  useEffect(() => {
    if (started === null) return;
    const ours = jobs.find((job) => job.id === started) ?? null;
    if (jobRunning(ours)) { seen.current = true; return; }
    if (ours === null && !seen.current) return;
    seen.current = false;
    setStarted(null);
    data.reload();
  }, [started, jobs]);
  if (root === null || insideHome(folder, root)) return null;
  const name = data.name || folderName(folder);

  const close = () => {
    setOpen(false);
    setAsking(false);
    setRefused(null);
  };

  const move = async (overrides: AddOverride[], chosen: AddFolderMode = way) => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const { stopped, result } = await stopThenMove(data.id, chosen, moveOverrides(overrides, everything, size), { stop: setTeamStopped, move: moveProjectHome });
    setBusy(false);
    setAsking(false);
    setRefused(null);
    const after = stopped ? " The team stays stopped; start it again from Team when you're ready." : "";
    if (result.kind === "not_ready") { setProblem("Moving a project isn't in this build."); return; }
    if (result.kind === "error") {
      if (stopped && asksAgain(result.code, overrides)) { setRefused(result.code); setAsking(true); return; }
      setProblem(`${moveCopy(result)}${after}`);
      return;
    }
    close();
    seen.current = false;
    setStarted(result.value.id);
    session?.refreshIntake(result.value);
    data.notify(`Moving ${name}. Follow it on Home; you can keep working.`);
  };

  const proceed = (chosen: AddFolderMode) => {
    if (anywayIssues(chosen, size, null).length > 0) setAsking(true);
    else void move([], chosen);
  };

  return <>
    <MoveHomeRow folder={folder} home={root} onOpen={() => { setProblem(""); setEverything(false); setOpen(true); }} />
    <MoveHomeDialog open={open && !asking} name={displayText(name)} folder={folder} home={root} way={way} size={size} busy={busy}
      problem={problem} everything={everything} onEverything={setEverything} onWay={setWay} onConfirm={() => proceed(way)} onCancel={close} />
    <AnywayDialog open={open && asking} way={way} name={displayText(name)} size={size} refused={refused} busy={busy} keepLabel="Keep it here"
      onUseHere={close} onAnyway={() => void move(anywayOverrides(way, anywayIssues(way, size, refused), size))}
      onCancel={() => { setAsking(false); setRefused(null); }} />
  </>;
}
