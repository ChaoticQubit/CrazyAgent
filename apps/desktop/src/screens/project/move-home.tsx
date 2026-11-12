import { useContext, useEffect, useRef, useState } from "react";
import { nextGeneration, setTeamStopped } from "../../api";
import type { Result } from "../../api";
import { ADD_WARNINGS, cancelIcloudDownload, downloadFromIcloud, intakeCopy, loadFolderSize, loadIcloudProgress, loadProjectsHome } from "../../api-intake";
import { moveProjectHome, receiptState } from "../../api-projects";
import type { StoredSetup } from "../../api-projects";
import type { AddFolderMode, AddOverride, FolderSize, IcloudProgress, IntentReceipt } from "../../engine-client";
import { SessionContext, useLoad, type Loaded } from "../../state";
import { Button, Dialog, Notice, Segmented } from "../../ui";
import { displayText } from "../conversation";
import { AnywayDialog, ICloudDialog } from "../intake/add-folder";
import {
  ICLOUD_PROGRESS_MS, anywayIssues, anywayOverrides, asksAgain, folderName, icloudOutcome, icloudPlaceholders, icloudRun, joinPath, sizeLine, worktreesLine,
} from "../intake/intake";
import { folderLabel, projectFolder } from "./overview";
import type { ProjectData } from "./shared";

const WAYS: { value: AddFolderMode; label: string; icon: "folder-input" | "copy" }[] = [
  { value: "move", label: "Move", icon: "folder-input" },
  { value: "copy", label: "Copy", icon: "copy" },
];

const MOVE_COPY: Record<string, string> = {
  move_project_busy: "The team is still stopping, or research or a change is still running. Move again in a few seconds.",
  move_project_master_session: "The master is still working in its own copy of this project. Let it finish, then move.",
  move_project_already_home: "This project is already in your CrazyAgent folder.",
  move_project_copy_worktrees: "An agent works in a git worktree of this project, which a copy would leave behind. Move the project instead.",
  move_project_folder_link: "This project's folder is reached through a link, so it can't be moved from here.",
  move_project_in_progress: "A project is being moved. Try again when it is done.",
  move_project_unavailable: "CrazyAgent couldn't check the team, so nothing was moved. Try again.",
  move_project_interrupted: "The move stopped part way. Check the project folder before trying again.",
  move_project_old_folder_left: "The project moved, but part of its old folder couldn't be deleted. Check the old folder and delete it yourself.",
  home_move_project: "Another project uses this folder too, so it can't be moved or copied.",
};

/** The code the engine answers when the project was moved and recorded in the projects folder,
 * and part of its old folder could not be deleted. It is a finished move, not a refusal. */
export const OLD_FOLDER_LEFT = "move_project_old_folder_left";

/** What the owner is told once a project is in the projects folder. Inputs: the project name,
 * move or copy, the old folder and whether part of it could not be deleted. Output: the notice,
 * which names the old folder when something is left in it. */
export function movedNotice(name: string, way: AddFolderMode, folder: string, left: boolean): string {
  const team = "The team is stopped; start it again from Team.";
  if (way === "copy") return `${name} now uses the copy in your CrazyAgent folder. ${team}`;
  if (!left) return `${name} is now in your CrazyAgent folder. ${team}`;
  return `${name} is now in your CrazyAgent folder. Some items in the old folder couldn't be deleted: check ${folderLabel(folder)} and delete it yourself. ${team}`;
}

/** Whether a project folder is the projects folder or inside it. Inputs: the project folder and
 * the projects folder, null when the owner has not picked one. Output: the answer. */
export function insideHome(folder: string, home: string | null): boolean {
  return home !== null && (folder === home || folder.startsWith(`${home}/`));
}

/** Plain sentence for one refusal of a project move. Input: the code, or the failure with the
 * engine cause behind it. Output: the move copy, else the add-folder and app copy. */
export function moveCopy(failure: string | { code: string; cause?: string }): string {
  const code = typeof failure === "string" ? failure : failure.code;
  return Object.hasOwn(MOVE_COPY, code) ? MOVE_COPY[code] : intakeCopy(failure);
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
  move: (id: string, mode: AddFolderMode, overrides: AddOverride[]) => Promise<Result<IntentReceipt>>;
}

/** Stops the project's team, then moves or copies the project, only once the stop was applied
 * (DEC-132 Q11). Inputs: the project id, move or copy, the soft refusals the owner chose to go
 * past and the two engine calls. Output: whether the team was stopped and the result of the
 * step that ended the run: the stop that was not applied, else the move. */
export async function stopThenMove(id: string, mode: AddFolderMode, overrides: AddOverride[], steps: MoveSteps): Promise<{ stopped: boolean; result: Result<IntentReceipt> }> {
  const stop = await steps.stop(id, true);
  if (stop.kind !== "ok" || receiptState(stop.value).kind !== "applied") return { stopped: false, result: stop };
  return { stopped: true, result: await steps.move(id, mode, overrides) };
}

/** The question before a project moves: what happens to the team and its work, Move or Copy,
 * where the folder goes, its size and what a move or copy can lose. Inputs: whether it is open,
 * the project name, its folder, the projects folder, the way, the folder read, a busy flag, the
 * last problem and the handlers. Output: the dialog, or nothing. */
export function MoveHomeDialog({ open, name, folder, home, way, size, busy, problem, onWay, onConfirm, onCancel }: {
  open: boolean;
  name: string;
  folder: string;
  home: string;
  way: AddFolderMode;
  size: Loaded<FolderSize> | null;
  busy: boolean;
  problem: string;
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
    {way === "copy" && <Notice tone="info">The copy becomes the project. The old folder stays where it is, with the agents' old copies in it.</Notice>}
    {way === "copy" && (read?.worktrees ?? 0) > 0 && <Notice tone="info">{worktreesLine(read?.worktrees ?? 0)}</Notice>}
    <Notice tone="warn">{loss.map((text) => <p key={text}>{text}</p>)}</Notice>
    {effects.length > 0 && <Notice tone="info">{effects.map((text) => <p key={text}>{text}</p>)}</Notice>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
  </Dialog>;
}

/** Move into CrazyAgent folder, for a project kept where the owner made it: the settings row,
 * the Move or Copy question, the add-folder "anyway" question for a folder with issues and the
 * iCloud download first for files only in iCloud. Confirming stops the team, then asks the engine
 * to move the folder and point the project and its agents' copies at the new place. Inputs: the
 * project data and its saved setup. Output: the row with its dialogs, or nothing for a project
 * already in the projects folder. */
export function MoveHome({ data, setup }: { data: Pick<ProjectData, "id" | "name" | "notify" | "reload">; setup: StoredSetup }) {
  const session = useContext(SessionContext);
  const [home] = useLoad(loadProjectsHome, [], "projects-home");
  const folder = projectFolder(setup.folder);
  const root = home.kind === "ok" ? home.value.path : null;
  const [open, setOpen] = useState(false);
  const [way, setWay] = useState<AddFolderMode>("move");
  const [size, setSize] = useState<Loaded<FolderSize> | null>(null);
  const [asking, setAsking] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [icloud, setIcloud] = useState<{ placeholders: number | null } | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<IcloudProgress | null>(null);
  const [stopping, setStopping] = useState(false);
  const run = useRef<ReturnType<typeof icloudRun> | null>(null);
  const downloaded = useRef(false);
  useEffect(() => () => run.current?.leave(), []);
  useEffect(() => {
    if (!open) return;
    let live = true;
    setSize({ kind: "loading" });
    void loadFolderSize(folder).then((read) => { if (live) setSize(read); });
    return () => { live = false; };
  }, [open, folder]);
  if (root === null || insideHome(folder, root)) return null;
  const name = data.name || folderName(folder);

  const close = () => {
    setOpen(false);
    setAsking(false);
    setRefused(null);
    setIcloud(null);
  };

  const move = async (overrides: AddOverride[], chosen: AddFolderMode = way) => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const { stopped, result } = await stopThenMove(data.id, chosen, overrides, {
      stop: setTeamStopped,
      move: (id, mode, sent) => moveProjectHome(id, mode, sent, nextGeneration()),
    });
    setBusy(false);
    setAsking(false);
    setRefused(null);
    const after = stopped ? " The team stays stopped; start it again from Team when you're ready." : "";
    if (result.kind === "not_ready") { setProblem("Moving a project isn't in this build."); return; }
    const state = result.kind === "ok" ? receiptState(result.value) : null;
    const code = result.kind === "error" ? result.code : state?.kind === "rejected" ? state.code : null;
    if (stopped && code !== null && asksAgain(code, overrides)) { setRefused(code); setAsking(true); return; }
    if (stopped && code === "home_icloud_not_downloaded" && !downloaded.current) { setIcloud({ placeholders: null }); return; }
    const left = code === OLD_FOLDER_LEFT;
    if (result.kind === "error" && !left) { setProblem(`${moveCopy(result)}${after}`); return; }
    if (state?.kind === "rejected" && !left) { setProblem(`${moveCopy(state)}${after}`); return; }
    if (state?.kind === "unconfirmed") { setProblem(`We couldn't confirm the move. Check the project folder before trying again.${after}`); return; }
    close();
    await session?.refreshProjects();
    data.notify(movedNotice(name, chosen, folder, left));
    data.reload();
  };

  const proceed = (chosen: AddFolderMode) => {
    if (anywayIssues(chosen, size, null).length > 0) setAsking(true);
    else void move([], chosen);
  };

  const confirm = () => {
    const placeholders = icloudPlaceholders(size);
    if (placeholders > 0 && !downloaded.current) setIcloud({ placeholders });
    else proceed(way);
  };

  const download = async (chosen: AddFolderMode) => {
    if (downloading) return;
    setWay(chosen);
    setDownloading(true);
    setProgress(null);
    setProblem("");
    const watching = icloudRun(
      () => void cancelIcloudDownload(),
      () => void loadIcloudProgress().then((read) => { if (read.kind === "ok" && read.value) setProgress(read.value); }),
      ICLOUD_PROGRESS_MS,
    );
    run.current = watching;
    const result = await downloadFromIcloud(folder);
    const outcome = icloudOutcome(watching.end(), result);
    run.current = null;
    if (outcome.next === "none") return;
    setDownloading(false);
    setStopping(false);
    setIcloud(null);
    if (outcome.next === "add") { downloaded.current = true; proceed(chosen); return; }
    setProblem(outcome.text);
  };

  return <>
    <MoveHomeRow folder={folder} home={root} onOpen={() => { setProblem(""); setOpen(true); }} />
    <MoveHomeDialog open={open && !asking && icloud === null} name={displayText(name)} folder={folder} home={root} way={way} size={size} busy={busy}
      problem={problem} onWay={setWay} onConfirm={confirm} onCancel={close} />
    <AnywayDialog open={open && asking} way={way} name={displayText(name)} size={size} refused={refused} busy={busy}
      onUseHere={close} onAnyway={() => void move(anywayOverrides(way, anywayIssues(way, size, refused), size))}
      onCancel={() => { setAsking(false); setRefused(null); }} />
    {icloud && <ICloudDialog open={open} name={displayText(name)} source={folder} placeholders={icloud.placeholders} way={way}
      downloading={downloading} progress={progress} stopping={stopping} onDownload={(chosen) => void download(chosen)}
      onStop={() => { setStopping(true); run.current?.stop(); }} onCancel={() => setIcloud(null)} />}
  </>;
}
