import { useEffect, useState, type ReactNode } from "react";
import { nextGeneration, type Result } from "../../api";
import { intakeCopy, loadFoundProjects, loadProjectsHome, startOnboarding } from "../../api-intake";
import { loadPools, receiptState, removeProject, type PoolChoice } from "../../api-projects";
import type { FoundProject, ProjectsHome } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, Icon, RadioCard, StatusBadge, formatRoute, navigate, type IconName } from "../../ui";
import { displayText } from "../conversation";
import { poolName } from "../new-project/wizard";
import { folderLabel } from "../project/overview";
import { NotReady } from "../project/shared";
import { HomeFolderPicker } from "./home-folder";
import { FOUND_POLL_MS, foundRow, homeLine, pendingFolders, startedProject, type FoundAction } from "./intake";
import "../project/projects.css";
import "./intake.css";

const ACTION_LABELS: Record<Exclude<FoundAction, null>, string> = {
  setup: "Set up this project", continue: "Continue setup", open: "Open project", remove: "Remove project",
};

const ACTION_ICONS: Record<Exclude<FoundAction, null>, IconName> = {
  setup: "list-checks", continue: "arrow-right", open: "folder-open", remove: "trash",
};

/** Home's projects folder section as a view of its reads: the chooser while no folder is set, then
 * the found, setting up and missing folders with one action each. Inputs: the two reads, an
 * optional chooser, and the action and retry handlers. Output: the section, or nothing to say. */
export function FoundProjectsView({ home, found, chooser, onSetUp, onOpen, onRemove, onRetry }: {
  home: Loaded<ProjectsHome>;
  found: Loaded<FoundProject[]>;
  chooser?: ReactNode;
  onSetUp: (folder: FoundProject) => void;
  onOpen: (id: string, setup: boolean) => void;
  onRemove: (folder: FoundProject) => void;
  onRetry: () => void;
}) {
  if (home.kind === "loading") return null;
  if (home.kind === "not_ready") return <section className="pj in-found"><NotReady what="the projects folder isn't in this build." /></section>;
  if (home.kind === "error") return <section className="pj in-found"><p className="pj-error" role="alert">{home.message}</p></section>;
  if (!home.value.path) return <section className="pj in-found in-panel" aria-labelledby="in-home-title">
    <h2 id="in-home-title">Choose your projects folder</h2>
    {chooser}
  </section>;
  const line = homeLine(home.value);
  const rows = found.kind === "ok" ? pendingFolders(found.value) : [];
  const act = (folder: FoundProject, action: FoundAction) => {
    const id = folder.project_id ?? null;
    if (action === "setup") onSetUp(folder);
    else if (action === "remove") onRemove(folder);
    else if (id && (action === "open" || action === "continue")) onOpen(id, action === "continue");
  };
  return <section className="pj in-found" aria-label="Projects folder">
    {line && <p className="pj-small pj-muted">{line}</p>}
    {found.kind === "not_ready" && <NotReady what="new project folders aren't listed in this build." />}
    {found.kind === "error" && <div className="pj-row" role="alert"><span className="pj-error">{found.message}</span>
      <Button size="small" icon="refresh-cw" onClick={onRetry}>Try again</Button></div>}
    {rows.length > 0 && <>
      <h2>In your projects folder</h2>
      <ul className="in-list">{rows.map((folder) => {
        const row = foundRow(folder);
        return <li className="in-row" key={folder.path}>
          <span className="in-row-main">
            <span className="pj-row"><Icon name={folder.git ? "git-branch" : "folder"} size={14} /><strong>{displayText(folder.name)}</strong><StatusBadge word={row.badge} tone={row.tone} /></span>
            <span className="pj-mono pj-muted" title={folder.path}>{displayText(folderLabel(folder.path))}</span>
            <span className="pj-small pj-secondary">{row.line} {folder.git ? "Git repository." : "No git yet."}</span>
          </span>
          {row.action && <Button size="small" variant={row.action === "setup" ? "primary" : row.action === "remove" ? "danger-text" : "secondary"}
            icon={ACTION_ICONS[row.action]} onClick={() => act(folder, row.action)}>{ACTION_LABELS[row.action]}</Button>}
        </li>;
      })}</ul>
    </>}
  </section>;
}

/** The Set up dialog body: what setting up does and the pool the mapping agent runs on.
 * Inputs: the folder, the pools read, the chosen pool id, its change handler and the last problem.
 * Output: the body. */
export function SetUpDialogBody({ folder, pools, pool, onPool, problem }: {
  folder: FoundProject;
  pools: Result<PoolChoice[]> | undefined;
  pool: string;
  onPool: (pool: string) => void;
  problem: string;
}) {
  const name = displayText(folder.name);
  return <div className="pj-stack-s">
    <p>CrazyAgent builds a code graph of {name}, then an agent on the pool you pick reads it and fills in the project details for you to check.</p>
    <p className="pj-secondary">Nothing in the folder changes except a hidden .crazyagent folder that git ignores. Research stays off until you turn it on.</p>
    {pools === undefined && <p className="pj-help" role="status">Loading your pools...</p>}
    {pools?.kind === "not_ready" && <NotReady what="pools aren't in this build." />}
    {pools?.kind === "error" && <p className="pj-error" role="alert">{pools.message}</p>}
    {pools?.kind === "ok" && pools.value.length === 0 && <p>Create a pool first. <a href={formatRoute({ name: "settings", tab: "pools", action: "new" })}>Create pool</a></p>}
    {pools?.kind === "ok" && pools.value.length > 0 && <fieldset className="pj-field">
      <legend>Pool</legend>
      {pools.value.map((choice) => <RadioCard key={choice.pool.id} name="in-pool" value={choice.pool.id} checked={pool === choice.pool.id} onChecked={onPool}
        title={displayText(poolName(choice.pool))} details={[`${choice.members.length} ${choice.members.length === 1 ? "account" : "accounts"}`]} />)}
    </fieldset>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
  </div>;
}

function SetUpDialog({ folder, onClose, onStarted }: { folder: FoundProject; onClose: () => void; onStarted: () => void }) {
  const [pools, setPools] = useState<Result<PoolChoice[]>>();
  const [pool, setPool] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => {
    let live = true;
    void loadPools().then((result) => {
      if (!live) return;
      setPools(result);
      if (result.kind === "ok" && result.value.length > 0) setPool((old) => old || result.value[0].pool.id);
    });
    return () => { live = false; };
  }, []);
  const start = async () => {
    if (!pool || busy) return;
    setBusy(true);
    setProblem("");
    const result = await startOnboarding(folder.path, pool, nextGeneration());
    setBusy(false);
    if (result.kind !== "ok") {
      setProblem(result.kind === "not_ready" ? "Setting up a found folder isn't in this build." : intakeCopy(result.code));
      return;
    }
    const started = startedProject(result.value);
    if (started.kind === "rejected") { setProblem(intakeCopy(started.code)); return; }
    if (started.kind === "started") { navigate({ name: "onboarding", id: started.id }); return; }
    if (receiptState(result.value).kind !== "applied") { setProblem("We couldn't confirm setup started. Check Home and try again."); return; }
    const found = await loadFoundProjects();
    const entry = found.kind === "ok" ? found.value.find((item) => item.path === folder.path && item.project_id) : undefined;
    if (entry?.project_id) navigate({ name: "onboarding", id: entry.project_id });
    else onStarted();
  };
  return <Dialog open title={`Set up ${folder.name}`} onClose={onClose}
    footer={<><Button onClick={onClose}>Cancel</Button>
      <Button variant="primary" icon="list-checks" disabled={!pool} pending={busy} pendingLabel="Starting..." onClick={() => void start()}>Start setup</Button></>}>
    <SetUpDialogBody folder={folder} pools={pools} pool={pool} onPool={setPool} problem={problem} />
  </Dialog>;
}

/** Home's projects folder section: reads the folder and its found projects every few seconds,
 * sets one up on a pool, opens or continues one, and removes a project whose folder is gone. */
export function FoundProjects() {
  const session = useAppSession();
  const [home, reloadHome] = useLoad(loadProjectsHome);
  const [found, setFound] = useState<Loaded<FoundProject[]>>({ kind: "loading" });
  const [tick, setTick] = useState(0);
  const [settingUp, setSettingUp] = useState<FoundProject | null>(null);
  const [removing, setRemoving] = useState<FoundProject | null>(null);
  const [busy, setBusy] = useState(false);
  const path = home.kind === "ok" ? home.value.path : null;

  useEffect(() => {
    if (!path) return;
    let live = true;
    let timer: number | undefined;
    const read = async () => {
      const result = await loadFoundProjects();
      if (!live) return;
      setFound(result);
      if (result.kind !== "not_ready") timer = window.setTimeout(() => void read(), FOUND_POLL_MS);
    };
    void read();
    return () => { live = false; window.clearTimeout(timer); };
  }, [path, tick]);

  const ready = found.kind === "ok" ? found.value.filter((folder) => folder.state === "ready").map((folder) => folder.project_id ?? folder.path).sort().join("\n") : "";
  useEffect(() => { if (ready) void session.refreshProjects(); }, [ready]);

  const remove = async (folder: FoundProject) => {
    if (!folder.project_id || busy) return;
    setBusy(true);
    const result = await removeProject(folder.project_id, nextGeneration());
    setBusy(false);
    setRemoving(null);
    if (result.kind !== "ok") { session.showToast(result.kind === "not_ready" ? "Removing a project isn't in this build." : intakeCopy(result.code)); return; }
    const state = receiptState(result.value);
    if (state.kind === "rejected") session.showToast(intakeCopy(state.code));
    else if (state.kind === "unconfirmed") session.showToast("We couldn't confirm the project was removed. Check Home.");
    else session.showToast(`${folder.name} was removed.`);
    void session.refreshProjects();
    setTick((old) => old + 1);
  };

  return <>
    <FoundProjectsView home={home} found={found} chooser={<HomeFolderPicker onSaved={reloadHome} />}
      onSetUp={setSettingUp} onRemove={setRemoving} onRetry={() => setTick((old) => old + 1)}
      onOpen={(id, setup) => navigate(setup ? { name: "onboarding", id } : { name: "project", id, tab: "overview" })} />
    {settingUp && <SetUpDialog folder={settingUp} onClose={() => setSettingUp(null)}
      onStarted={() => { setSettingUp(null); setTick((old) => old + 1); session.showToast(`Setting up ${settingUp.name}. Continue setup from Home.`); }} />}
    <ConfirmDialog open={removing !== null} title={`Remove ${removing?.name ?? "this project"}?`} confirmLabel="Remove project" cancelLabel="Keep project" pending={busy}
      onCancel={() => setRemoving(null)} onConfirm={() => { if (removing) void remove(removing); }}>
      CrazyAgent forgets this project. Its folder is no longer in your projects folder, so nothing on disk changes.
    </ConfirmDialog>
  </>;
}
