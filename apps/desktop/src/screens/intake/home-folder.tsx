import { useState } from "react";
import { nextGeneration, pickFolder } from "../../api";
import { intakeCopy, loadProjectsHome, setProjectsHome } from "../../api-intake";
import { receiptState } from "../../api-projects";
import type { ProjectsHome } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { Button, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { folderLabel } from "../project/overview";
import { NotReady } from "../project/shared";
import { pickedFolderProblem } from "./intake";
import "../project/projects.css";
import "./intake.css";

/** The projects folder chooser as a view of its state. Inputs: the projects home read, the folder
 * picked and not yet saved, a busy flag, the last problem, and the Choose, Save and optional
 * Continue handlers. Output: the chooser. */
export function HomeFolderView({ home, picked, busy, problem, onChoose, onSave, onContinue }: {
  home: Loaded<ProjectsHome>;
  picked: string | null;
  busy: boolean;
  problem: string;
  onChoose: () => void;
  onSave: () => void;
  onContinue?: () => void;
}) {
  if (home.kind === "loading") return <Skeleton shape="line" count={3} />;
  if (home.kind === "not_ready") return <div className="pj-stack-s">
    <NotReady what="choosing a projects folder isn't in this build." />
    {onContinue && <Button onClick={onContinue}>Continue</Button>}
  </div>;
  if (home.kind === "error") return <div className="pj-stack-s">
    <p className="pj-error" role="alert">{home.message}</p>
    {onContinue && <Button onClick={onContinue}>Continue</Button>}
  </div>;
  const current = home.value.path;
  return <div className="pj-stack-s">
    <p className="pj-secondary">CrazyAgent keeps each project in its own folder inside this one. Drop a project folder into it with Finder and it shows up on Home.</p>
    {current && !picked && <p>Your projects folder is <span className="pj-mono" title={current}>{displayText(folderLabel(current))}</span>.</p>}
    {picked && <p>Use <span className="pj-mono" title={picked}>{displayText(folderLabel(picked))}</span>
      {current ? ". Projects already in the old folder stay there." : "."}</p>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <span className="pj-row">
      <Button icon="folder-open" disabled={busy} onClick={onChoose}>{current || picked ? "Choose another folder" : "Choose folder"}</Button>
      {picked && <Button variant="primary" icon="check" pending={busy} pendingLabel="Saving..." onClick={onSave}>Use this folder</Button>}
      {!picked && current && onContinue && <Button variant="primary" onClick={onContinue}>Continue</Button>}
    </span>
  </div>;
}

/** Reads the projects folder, lets the owner pick one in the macOS chooser and saves it; nothing
 * on disk moves. Inputs: what to do once a folder is saved and an optional Continue for first run.
 * Output: the chooser. */
export function HomeFolderPicker({ onSaved, onContinue }: { onSaved?: () => void; onContinue?: () => void }) {
  const [home, reload] = useLoad(loadProjectsHome, [], "projects-home");
  const [picked, setPicked] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");

  const choose = async () => {
    setProblem("");
    const result = await pickFolder();
    if (result.kind !== "ok") {
      setProblem(result.kind === "not_ready" ? "The folder chooser isn't in this build." : result.message);
      return;
    }
    if (result.value === null) return;
    const issue = pickedFolderProblem(result.value, null);
    if (issue) setProblem(issue);
    else setPicked(result.value);
  };

  const save = async () => {
    if (!picked || busy) return;
    setBusy(true);
    const result = await setProjectsHome(picked, nextGeneration());
    setBusy(false);
    if (result.kind !== "ok") {
      setProblem(result.kind === "not_ready" ? "Saving the projects folder isn't in this build." : intakeCopy(result));
      return;
    }
    const state = receiptState(result.value);
    if (state.kind === "rejected") { setProblem(intakeCopy(state)); return; }
    if (state.kind === "unconfirmed") { setProblem("We couldn't confirm the folder was saved. Check it and save again."); reload(); return; }
    setPicked(null);
    reload();
    onSaved?.();
  };

  return <HomeFolderView home={home} picked={picked} busy={busy} problem={problem} onChoose={() => void choose()} onSave={() => void save()} onContinue={onContinue} />;
}
