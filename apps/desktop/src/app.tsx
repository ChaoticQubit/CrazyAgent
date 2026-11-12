import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { SessionContext, useLoad, useSession, type Loaded, type Session } from "./state";
import { engineVersions, plainError, unconfirmedLine, PROFILE_UNCONFIRMED_LINE, STUCK_SIGN_IN_LINE, type EngineVersions } from "./api";
import { ideFocus, type IdeFocus } from "./engine-client";
import { loadTeamEvents, teamAttention } from "./api-projects";
import { loadQuestionCounts, pendingByProject, unseenQuestionAlerts, withPendingQuestions } from "./api-questions";
import { questionWatch } from "./notify";
import { displayText } from "./screens/conversation";
import { Home } from "./screens/dashboard";
import { Welcome } from "./screens/first-run";
import { AddFolder } from "./screens/intake/add-folder";
import { CloneFromGitHub } from "./screens/intake/clone";
import { OnboardingReview } from "./screens/intake/onboarding";
import { AppMetrics } from "./screens/metrics";
import { useResetNotice } from "./screens/metrics/extras";
import { NewProjectFlow } from "./screens/new-project";
import { ProjectPage } from "./screens/project";
import { Settings } from "./screens/settings";
import { alertRoute, freshAlerts, unseenByProject, workingByProject } from "./screens/team/wake";
import { RestartEngine, UpdatesSettings, useEngineRestart, useUpdateWatch, type EngineRestart } from "./screens/settings/updates";
import { blocking, updateStore } from "./screens/settings/updates-state";
import { AppShell, Banner, BrandMark, Button, Icon, Sidebar, Skeleton, Toast, navigate, shortcutKey, useRoute, type Route, type SidebarProject } from "./ui";
import { GoToLayer } from "./ui/go-to";

/** Support details the engine-down screen copies, like the TUI's: what is wrong, the window
 * size and the time, never paths, accounts or secrets. Inputs: the window size and the time.
 * Output: plain text lines. */
export function supportDetails(width: number, height: number, now: Date): string {
  return `CrazyAgent desktop\nEngine: not running\nProblem: engine_unreachable\nWindow: ${Math.round(width)}x${Math.round(height)}\nTime: ${now.toISOString()}\n`;
}

const UPDATES_ROUTE: Route = { name: "settings", tab: "updates" };

/** The way to Settings, Updates from a full-window state that needs no engine, naming a
 * release a check already found. Output: the line and the Check for updates button. */
function UpdateLink() {
  const update = useSyncExternalStore(updateStore.subscribe, updateStore.get, updateStore.get);
  return <>
    {update.kind === "available" && <p>CrazyAgent {displayText(update.update.version)} is available.</p>}
    <span><Button variant="quiet" icon="refresh-cw" onClick={() => navigate(UPDATES_ROUTE)}>Check for updates</Button></span>
  </>;
}

function EngineDown({ onRetry }: { onRetry: () => void }) {
  const [copied, setCopied] = useState<"idle" | "copied" | { failed: string }>("idle");
  async function copy() {
    const details = supportDetails(window.innerWidth, window.innerHeight, new Date());
    try {
      await navigator.clipboard.writeText(details);
      setCopied("copied");
    } catch {
      setCopied({ failed: details });
    }
  }
  return <main id="main" className="full-state">
    <div className="box">
      <BrandMark />
      <Icon name="wifi-off" size={28} />
      <h1>CrazyAgent's engine isn't running</h1>
      <p className="secondary">Projects, accounts and agents live in the engine. Start it, then try again.</p>
      <span className="row"><Button variant="primary" icon="refresh-cw" onClick={onRetry}>Try again</Button>
        <Button icon="copy" onClick={() => void copy()}>Copy details for support</Button></span>
      <UpdateLink />
      {copied === "copied" && <p className="secondary" role="status">Copied. Paste it into your message to support.</p>}
      {typeof copied === "object" && <><p className="secondary" role="alert">Couldn't copy. Select the details below and copy them.</p>
        <pre className="support-details">{copied.failed}</pre></>}
    </div>
  </main>;
}

/** Settings, Updates on its own, for when the engine is down, still starting, or first-run
 * setup is unfinished, so the owner can always check for and install an update. Input: the
 * session. Output: the full-window page with Back. */
function UpdatesAlone({ session }: { session: Session }) {
  return <SessionContext.Provider value={session}>
    <main id="main" className="full-state">
      <div className="box wide">
        <BrandMark />
        <h1>Updates</h1>
        <UpdatesSettings />
        <span><Button icon="arrow-left" onClick={() => navigate({ name: "home" })}>Back</Button></span>
      </div>
    </main>
    <Toast message={session.toast} />
  </SessionContext.Provider>;
}

/** The full-window state for an engine of another version than this app: the engine's own
 * message naming both versions, Restart engine on this app's version, Try again, and Check
 * for updates, since a newer app can also end the mismatch. Inputs: the version read, the
 * restart state, the handlers and whether an update install holds Restart engine off. */
export function EngineOutdatedView({ versions, restart, onRestart, onRetry, installing = false }: {
  versions: Loaded<EngineVersions>;
  restart: EngineRestart;
  onRestart: () => void;
  onRetry: () => void;
  installing?: boolean;
}) {
  const message = versions.kind === "ok" && versions.value.message ? displayText(versions.value.message) : plainError("engine_version_mismatch");
  return <main id="main" className="full-state">
    <div className="box">
      <BrandMark />
      <Icon name="refresh-cw" size={28} />
      <h1>CrazyAgent's engine needs a restart</h1>
      <p className="secondary">{message}</p>
      <RestartEngine restart={restart} onRestart={onRestart} primary disabled={installing} />
      <span><Button icon="refresh-cw" onClick={onRetry}>Try again</Button></span>
      <UpdateLink />
    </div>
  </main>;
}

function EngineOutdated({ onRetry }: { onRetry: () => void }) {
  const [versions] = useLoad(engineVersions);
  const [restart, restartNow] = useEngineRestart(onRetry);
  const update = useSyncExternalStore(updateStore.subscribe, updateStore.get, updateStore.get);
  return <EngineOutdatedView versions={versions} restart={restart} onRestart={restartNow} onRetry={onRetry} installing={blocking(update)} />;
}

function Launching() {
  return <div className="shell" aria-busy="true">
    <aside className="sidebar"><BrandMark /><Skeleton shape="line" count={6} /></aside>
    <main id="main" className="main"><div className="page"><div className="page-header"><Skeleton shape="line" /></div><Skeleton shape="card" count={6} /></div></main>
  </div>;
}

/** The routed screen inside the shell. Each project page and onboarding review is keyed by its
 * project id, so a switch to another project starts a fresh page and never keeps the last
 * project's master ticket, name, settings or late reads. Inputs: the route, the session and
 * each project's unseen team notification count for Home. Output: the screen element. */
export function Screen({ route, session, alerts }: { route: Route; session: Session; alerts?: Map<string, number> }) {
  const projects = session.projects.kind === "ok" ? session.projects.value : undefined;
  switch (route.name) {
    case "settings": return <Settings tab={route.tab} action={route.action} account={route.account} skill={route.skill} />;
    case "new-project": return <NewProjectFlow step={route.step} />;
    case "add-project": return route.way === "clone" ? <CloneFromGitHub /> : <AddFolder />;
    case "onboarding": return <OnboardingReview key={route.id} id={route.id} />;
    case "metrics": return <AppMetrics projects={projects ?? []} />;
    case "project": return <ProjectPage key={route.id} id={route.id} tab={route.tab} name={projects?.find((project) => project.id === route.id)?.name} />;
    default: return <Home alerts={alerts} />;
  }
}

/** The shell's sidebar with the engine restart its footer menu runs: the same restart
 * Settings, Updates uses, held off while an update install runs, and the session reconnects
 * once the engine is back. Inputs: the route, the projects, the profile name and the session. */
function ShellSidebar({ route, projects, profileName, session }: { route: Route; projects: SidebarProject[] | undefined; profileName: string | undefined; session: Session }) {
  const [restart, restartNow] = useEngineRestart(session.retry);
  const update = useSyncExternalStore(updateStore.subscribe, updateStore.get, updateStore.get);
  const control = { held: blocking(update), busy: restart.kind === "busy", failure: restart.kind === "failed" ? restart.message : "", onRestart: restartNow };
  return <Sidebar route={route} projects={projects} profileName={profileName} engine={session.engine} onRetry={session.retry} restart={control} />;
}

/** The whole app for one session and route: the engine-down or engine-outdated state, the
 * launch skeleton, the first-run Welcome page while the profile is empty, or the shell with the
 * routed screen. Settings, Updates opens on its own in place of all but the shell. Inputs: the
 * session and the current route. */
export function AppView({ session, route, alerts, working }: { session: Session; route: Route; alerts?: Map<string, number>; working?: ReadonlyMap<string, number> }) {
  const updates = route.name === "settings" && route.tab === "updates";
  if (session.engine === "down") return updates ? <UpdatesAlone session={session} /> : <EngineDown onRetry={session.retry} />;
  if (session.engine === "outdated") return updates ? <UpdatesAlone session={session} /> : <EngineOutdated onRetry={session.retry} />;
  if (session.engine === "connecting" || session.profile.kind === "loading") return updates ? <UpdatesAlone session={session} /> : <Launching />;
  const profile = session.profile.kind === "ok" ? session.profile.value : undefined;
  if (session.profile.kind === "ok" && profile === null && updates) return <UpdatesAlone session={session} />;
  if (session.profile.kind === "ok" && profile === null || route.name === "welcome") {
    return <SessionContext.Provider value={session}>
      <Welcome onSaved={(_, next) => { void session.refreshProfile().then(() => navigate(next ?? { name: "home" }, true)); }}
        onSkip={(next) => navigate(next ?? { name: "home" }, true)} />
    </SessionContext.Provider>;
  }
  const toastOpen = session.toastOpen;
  const projects = session.projects.kind === "ok" ? session.projects.value.map((project) => {
    const unseen = alerts?.get(project.id) ?? 0;
    return { id: project.id, name: displayText(project.name), flag: unseen > 0 ? String(unseen) : undefined, working: working?.get(project.id) };
  }) : undefined;
  return <SessionContext.Provider value={session}>
    <AppShell toast={<Toast message={session.toast} onOpen={toastOpen ? () => navigate(toastOpen) : undefined} />}
      sidebar={<ShellSidebar route={route} projects={projects} profileName={profile?.name} session={session} />}>
      {session.unconfirmed && <Banner action={<span className="row">
        <Button size="small" pending={session.unconfirmed.busy} pendingLabel="Checking..." onClick={() => void session.checkUnconfirmed()}>Check again</Button>
        <Button size="small" disabled={session.unconfirmed.busy} onClick={() => void session.retryUnconfirmed()}>Retry</Button>
        {session.unconfirmed.outcome === "not_recorded_at_barrier" &&
          <Button size="small" disabled={session.unconfirmed.busy} onClick={() => void session.dropUnconfirmed()}>Dismiss</Button>}
      </span>}>
        {unconfirmedLine(session.unconfirmed.intent, session.unconfirmed.outcome)}
      </Banner>}
      {session.profileCheck && <Banner action={<Button size="small" pending={session.profileCheck.busy} pendingLabel="Checking..."
        onClick={() => void session.checkProfile()}>Check again</Button>}>
        {PROFILE_UNCONFIRMED_LINE}
      </Banner>}
      {session.stuckSignIn && <Banner action={<Button size="small" pending={session.stuckSignIn.busy} pendingLabel="Clearing..."
        onClick={() => void session.clearStuckSignIn()}>Clear stuck sign-in</Button>}>
        {STUCK_SIGN_IN_LINE}
      </Banner>}
      <Screen route={route} session={session} alerts={alerts} />
      <GoToLayer projects={projects ?? []} />
    </AppShell>
  </SessionContext.Provider>;
}

function useShortcuts() {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!shortcutKey(event)) return;
      if (event.key === "n") { event.preventDefault(); navigate({ name: "new-project", step: "basics" }); }
      else if (event.key === ",") { event.preventDefault(); navigate({ name: "settings", tab: "accounts" }); }
      else if (event.key === "[") { event.preventDefault(); window.history.back(); }
      else if (event.key === "]") { event.preventDefault(); window.history.forward(); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

export const ALERT_READ_MS = 10_000;

const NONE_WORKING: ReadonlyMap<string, number> = new Map();

/** What the sidebar shows from the team alert read: each project's flag count and its agents
 * at work. */
export interface TeamAlerts {
  alerts: Map<string, number>;
  working: ReadonlyMap<string, number>;
}

/** Watches the owner's team notifications and pending questions while the engine runs: every
 * 10 s it reads them, shows a new notification as a toast, with Open to the Agents tab's
 * Questions for you drawer on an urgent question or an agent waiting in its terminal, keeps
 * the app icon badge at the unseen notifications plus the pending questions, and asks for
 * attention when a notification is new; a Normal question only counts toward the badge. The
 * same counts read drives the question notification watch, so the counts are read once per
 * look. Inputs: whether the engine runs and the toast action. Output: what each project's
 * sidebar flag counts and how many agents each has at work, none when a read fails or the
 * engine stops. */
export function useTeamAlerts(running: boolean, showToast: (message: string, open?: Route) => void): TeamAlerts {
  const [counts, setCounts] = useState<Map<string, number>>(() => new Map());
  const [working, setWorking] = useState<ReadonlyMap<string, number>>(NONE_WORKING);
  const toast = useRef(showToast);
  toast.current = showToast;
  useEffect(() => {
    if (!running) {
      setWorking(NONE_WORKING);
      return;
    }
    let live = true;
    let last: number | null = null;
    let shown = 0;
    let timer = 0;
    const watch = questionWatch((line) => toast.current(line));
    const read = async () => {
      const [result, questions] = await Promise.all([loadTeamEvents(), loadQuestionCounts()]);
      if (!live) return;
      const pending = questions.kind === "ok" ? pendingByProject(questions.value) : new Map<string, number>();
      if (result.kind === "ok") {
        const next = freshAlerts(last, result.value);
        last = next.last;
        const flags = withPendingQuestions(unseenByProject(result.value), unseenQuestionAlerts(result.value.projects), pending);
        const total = [...flags.values()].reduce((sum, count) => sum + count, 0);
        setCounts(flags);
        setWorking(workingByProject(result.value));
        if (next.fresh[0]) toast.current(displayText(next.fresh[0].text), alertRoute(next.fresh[0]));
        if (next.fresh.length > 0 || total !== shown) void teamAttention(total, next.fresh.length > 0);
        shown = total;
      } else {
        setWorking(NONE_WORKING);
      }
      if (questions.kind === "ok") await watch.look(questions.value);
      if (live && (result.kind !== "not_ready" || questions.kind !== "not_ready")) timer = window.setTimeout(read, ALERT_READ_MS);
    };
    void read();
    return () => {
      live = false;
      window.clearTimeout(timer);
      watch.stop();
    };
  }, [running]);
  return { alerts: counts, working };
}

export const IDE_FOCUS_POLL_MS = 1_000;

/** The route a TUI "open in IDE" brings forward: the project's IDE tab.
 * Input: the focus the engine handed over. Output: the route. */
export function ideFocusRoute(focus: IdeFocus): Route {
  return { name: "project", id: focus.project, tab: "ide" };
}

/** Brings forward the IDE tab a TUI "open in IDE" asked for: while the engine runs, every
 * second it takes the waiting focus, if any, and opens that project's IDE tab; the engine
 * raises the window. The desktop asks the engine only while an IDE webview is loaded, since
 * only a loaded IDE can take a reveal. Input: whether the engine runs. */
function useIdeFocus(running: boolean) {
  useEffect(() => {
    if (!running) return;
    let live = true;
    let timer = 0;
    const read = async () => {
      const focus = await ideFocus().catch(() => null);
      if (!live) return;
      if (focus) navigate(ideFocusRoute(focus));
      timer = window.setTimeout(read, IDE_FOCUS_POLL_MS);
    };
    void read();
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [running]);
}

/** The desktop app: one engine session, the hash route, the app-wide shortcuts, the team
 * notification and question watch, the new free reset notice and the TUI's "open in IDE"
 * focus. */
export function App() {
  const session = useSession();
  const route = useRoute();
  useShortcuts();
  useUpdateWatch(session.showToast);
  const { alerts, working } = useTeamAlerts(session.engine === "running", session.showToast);
  useResetNotice(session.engine === "running", session.showToast);
  useIdeFocus(session.engine === "running");
  return <AppView session={session} route={route} alerts={alerts} working={working} />;
}
