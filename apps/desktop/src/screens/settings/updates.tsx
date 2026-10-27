import { useEffect, useState, useSyncExternalStore } from "react";
import { engineVersions, restartEngine, type EngineVersions, type Failure } from "../../api";
import { checkForUpdate, installUpdate, linkCommandLineTool, loadUpdateSettings, restartApp, setUpdateChannel, type UpdateChannel, type UpdateSettings } from "../../api-updates";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, Icon, LoadError, NotReady, Segmented, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { CHECK_INTERVAL_MS, progressLabel, progressPercent, shouldAnnounce, updateMessage, updateStore, type UpdatePhase } from "./updates-state";
import "../project/projects.css";
import "../intake/intake.css";

export type CliState = { kind: "idle" } | { kind: "busy" } | { kind: "done"; message: string } | { kind: "failed"; message: string };
export type EngineRestart = { kind: "idle" } | { kind: "busy" } | { kind: "failed"; message: string };

const CHANNELS: { value: UpdateChannel; label: string }[] = [
  { value: "stable", label: "Stable" },
  { value: "dev", label: "Dev" },
];

const failure = (result: Failure, what: string) =>
  result.kind === "not_ready" ? `${what} isn't in this build.` : updateMessage(result.code);

function checkedLine(at: number): string {
  return `Checked at ${new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}.`;
}

function UpdateStatus({ phase, onCheck, onInstall, onRestart }: {
  phase: UpdatePhase;
  onCheck: () => void;
  onInstall: () => void;
  onRestart: () => void;
}) {
  const check = (label: string) => <Button icon="search" pending={phase.kind === "checking"} pendingLabel="Checking..." onClick={onCheck}>{label}</Button>;
  switch (phase.kind) {
    case "idle":
    case "checking":
      return <><p className="pj-secondary">CrazyAgent checks for updates when it starts and every 6 hours. It never installs one without you.</p>
        <span>{check("Check now")}</span></>;
    case "current":
      return <><p>CrazyAgent is up to date. <span className="pj-muted">{checkedLine(phase.checkedAt)}</span></p>
        <span>{check("Check now")}</span></>;
    case "available":
      return <>
        <p>CrazyAgent {displayText(phase.update.version)} is available. You have {displayText(phase.update.current_version)}.</p>
        {phase.update.notes && <p className="pj-secondary" style={{ whiteSpace: "pre-wrap" }}>{displayText(phase.update.notes)}</p>}
        <p className="pj-small pj-muted">Installing stops the engine for a moment. Your projects, memory and settings stay, and agents pick up where they were.</p>
        <span className="pj-row">
          <Button variant="primary" icon="shield-check" onClick={onInstall}>Download and install</Button>
          {check("Check again")}
        </span>
      </>;
    case "installing": {
      const percent = progressPercent(phase.step);
      return <>
        <p>Installing CrazyAgent {displayText(phase.update.version)}.</p>
        <progress max={100} value={percent ?? undefined} aria-label="Update progress">{percent === null ? "" : `${percent}%`}</progress>
        <p className="pj-secondary" role="status">{progressLabel(phase.step)}</p>
      </>;
    }
    case "installed":
      return <>
        <p>CrazyAgent {displayText(phase.update.version)} is installed. Restart to finish.</p>
        {!phase.engineStarted && <p className="pj-secondary">The engine starts again when CrazyAgent restarts.</p>}
        <span><Button variant="primary" icon="refresh-cw" onClick={onRestart}>Restart now</Button></span>
      </>;
    case "failed":
      return <>
        <p className="pj-error" role="alert">{phase.message}</p>
        <span className="pj-row">
          {phase.update
            ? <Button variant="primary" icon="shield-check" onClick={onInstall}>Try again</Button>
            : <Button icon="search" onClick={onCheck}>Try again</Button>}
        </span>
      </>;
  }
}

/** Restart engine with its outcome: the plain error of a failed restart above the button.
 * Inputs: the restart state, the handler and whether the button is the page's main action. */
export function RestartEngine({ restart, onRestart, primary = false }: { restart: EngineRestart; onRestart: () => void; primary?: boolean }) {
  return <>
    {restart.kind === "failed" && <p className="pj-error" role="alert">{restart.message}</p>}
    <span><Button variant={primary ? "primary" : "secondary"} icon="refresh-cw" pending={restart.kind === "busy"} pendingLabel="Restarting..." onClick={onRestart}>Restart engine</Button></span>
  </>;
}

/** The engine's version under this app's and, when they differ, the engine's message with
 * Restart engine. Nothing shows until the version is read. Inputs: the read, the restart state
 * and the handler. */
export function EngineVersionLine({ versions, restart, onRestart }: { versions: Loaded<EngineVersions>; restart: EngineRestart; onRestart: () => void }) {
  if (versions.kind !== "ok") return null;
  const { engine, message } = versions.value;
  return <>
    <p className="pj-secondary">{engine === null ? "The engine isn't answering." : `Engine ${displayText(engine)}`}</p>
    {message && <><p role="status">{displayText(message)}</p><RestartEngine restart={restart} onRestart={onRestart} /></>}
  </>;
}

/** Restarts the default engine on this app's version on the owner's click. Input: what to do
 * once it runs again. Output: the restart state and the handler. */
export function useEngineRestart(onDone: () => void): [EngineRestart, () => void] {
  const [restart, setRestart] = useState<EngineRestart>({ kind: "idle" });
  const run = async () => {
    if (restart.kind === "busy") return;
    setRestart({ kind: "busy" });
    const result = await restartEngine();
    if (result.kind === "ok") {
      setRestart({ kind: "idle" });
      onDone();
    } else {
      setRestart({ kind: "failed", message: result.kind === "error" ? result.message : "Restarting the engine isn't in this build." });
    }
  };
  return [restart, () => void run()];
}

/** Settings, Updates as a view of its state: this app's version and channel with the channel
 * switch, the engine's version with Restart engine when it differs, the check and install
 * status with progress, Restart, and Install command line tool. Inputs: the settings and
 * engine version reads, the update phase, the channel, engine restart and command line tool
 * states and the handlers. Output: the page. */
export function UpdatesView({ settings, engine, phase, channelBusy, channelProblem, cli, engineRestart, onChannel, onCheck, onInstall, onRestart, onLinkCli, onRestartEngine, onRetry }: {
  settings: Loaded<UpdateSettings>;
  engine: Loaded<EngineVersions>;
  engineRestart: EngineRestart;
  onRestartEngine: () => void;
  phase: UpdatePhase;
  channelBusy: boolean;
  channelProblem: string;
  cli: CliState;
  onChannel: (channel: UpdateChannel) => void;
  onCheck: () => void;
  onInstall: () => void;
  onRestart: () => void;
  onLinkCli: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <Skeleton shape="line" count={3} />;
  if (settings.kind === "not_ready") return <NotReady what="Updates" />;
  if (settings.kind === "error") return <LoadError message={settings.code === "unknown" ? settings.message : updateMessage(settings.code)} onRetry={onRetry} />;
  const { current_version, channel } = settings.value;
  const busy = phase.kind === "installing" || phase.kind === "installed";
  return <div className="pj pj-stack">
    <section className="pj-stack-s in-panel" aria-label="Version and channel">
      <h2 className="row"><Icon name="shield-check" size={16} />CrazyAgent {displayText(current_version)}</h2>
      <EngineVersionLine versions={engine} restart={engineRestart} onRestart={onRestartEngine} />
      <p className="pj-secondary">Stable gets tested releases. Dev gets every change as soon as it's merged.</p>
      {busy || channelBusy
        ? <p>Channel: {channel === "dev" ? "Dev" : "Stable"}</p>
        : <Segmented label="Update channel" value={channel} options={CHANNELS} onValue={onChannel} />}
      {channelProblem && <p className="pj-error" role="alert">{channelProblem}</p>}
    </section>
    <section className="pj-stack-s in-panel" aria-label="Updates">
      <h2 className="row"><Icon name="refresh-cw" size={16} />Updates</h2>
      <UpdateStatus phase={phase} onCheck={onCheck} onInstall={onInstall} onRestart={onRestart} />
    </section>
    <section className="pj-stack-s in-panel" aria-label="Command line tool">
      <h2 className="row"><Icon name="square-terminal" size={16} />Command line tool</h2>
      <p className="pj-secondary">Use crazyagent in Terminal, for example crazyagent tui. It talks to the same engine as this app.</p>
      {cli.kind === "done" && <p role="status">{displayText(cli.message)}</p>}
      {cli.kind === "failed" && <p className="pj-error" role="alert">{cli.message}</p>}
      <span><Button icon="square-terminal" pending={cli.kind === "busy"} pendingLabel="Installing..." onClick={onLinkCli}>Install command line tool</Button></span>
    </section>
  </div>;
}

/** Checks for updates when the app starts and every 6 hours, and tells the owner once per new
 * version. It never installs. Input: how to tell the owner. */
export function useUpdateWatch(announce: (message: string) => void) {
  useEffect(() => {
    let live = true;
    let timer = 0;
    let announced: string | null = null;
    const run = async () => {
      updateStore.dispatch({ kind: "check_started" });
      const result = await checkForUpdate();
      if (!live) return;
      if (result.kind === "ok") {
        updateStore.dispatch({ kind: "check_done", update: result.value, at: Date.now() });
        if (shouldAnnounce(announced, result.value) && result.value) {
          announced = result.value.version;
          announce(`CrazyAgent ${result.value.version} is ready. Install it in Settings, Updates.`);
        }
      } else if (result.kind === "error") {
        updateStore.dispatch({ kind: "check_failed", code: result.code });
      }
      timer = window.setTimeout(() => void run(), CHECK_INTERVAL_MS);
    };
    void run();
    return () => { live = false; window.clearTimeout(timer); };
  }, []);
}

/** Settings, Updates: reads the version and channel, switches the channel, checks, downloads
 * and installs only on the owner's click, restarts, and installs the command line tool. */
export function UpdatesSettings() {
  const session = useAppSession();
  const [settings, reload] = useLoad(loadUpdateSettings, [], "update-settings");
  const [engine, reloadEngine] = useLoad(engineVersions, [], "engine-versions");
  const [engineRestart, restartEngineNow] = useEngineRestart(() => {
    reloadEngine();
    session.showToast("The engine restarted.");
  });
  const phase = useSyncExternalStore(updateStore.subscribe, updateStore.get, updateStore.get);
  const [channelBusy, setChannelBusy] = useState(false);
  const [channelProblem, setChannelProblem] = useState("");
  const [cli, setCli] = useState<CliState>({ kind: "idle" });

  const check = async () => {
    if (phase.kind === "checking") return;
    updateStore.dispatch({ kind: "check_started" });
    const result = await checkForUpdate();
    if (result.kind === "ok") updateStore.dispatch({ kind: "check_done", update: result.value, at: Date.now() });
    else updateStore.dispatch({ kind: "check_failed", code: result.kind === "error" ? result.code : "unknown" });
  };

  const install = async () => {
    updateStore.dispatch({ kind: "install_started" });
    const result = await installUpdate((progress) => updateStore.dispatch({ kind: "progress", progress }));
    if (result.kind === "ok") updateStore.dispatch({ kind: "install_done", engineStarted: result.value.engine_started });
    else updateStore.dispatch({ kind: "install_failed", code: result.kind === "error" ? result.code : "unknown" });
  };

  const switchChannel = async (channel: UpdateChannel) => {
    if (channelBusy || settings.kind !== "ok" || settings.value.channel === channel) return;
    setChannelBusy(true);
    setChannelProblem("");
    const result = await setUpdateChannel(channel);
    setChannelBusy(false);
    if (result.kind !== "ok") { setChannelProblem(failure(result, "Changing the channel")); return; }
    updateStore.dispatch({ kind: "channel_changed" });
    reload();
    session.showToast(channel === "dev" ? "You'll get Dev updates." : "You'll get Stable updates.");
    void check();
  };

  const restart = async () => {
    const result = await restartApp();
    if (result.kind !== "ok") session.showToast(failure(result, "Restarting"));
  };

  const linkCli = async () => {
    if (cli.kind === "busy") return;
    setCli({ kind: "busy" });
    const result = await linkCommandLineTool();
    setCli(result.kind === "ok"
      ? { kind: "done", message: result.value.trim() || "The command line tool is installed. Open a new Terminal window to use it." }
      : { kind: "failed", message: failure(result, "Installing the command line tool") });
  };

  return <UpdatesView settings={settings} engine={engine} phase={phase} channelBusy={channelBusy} channelProblem={channelProblem} cli={cli} onRetry={reload}
    engineRestart={engineRestart} onRestartEngine={restartEngineNow}
    onChannel={(channel) => void switchChannel(channel)} onCheck={() => void check()} onInstall={() => void install()}
    onRestart={() => void restart()} onLinkCli={() => void linkCli()} />;
}
