import { useEffect, useState } from "react";
import { SessionContext, useSession, type Session } from "./state";
import { Home } from "./screens/dashboard";
import { Welcome } from "./screens/first-run";
import { AddFolder } from "./screens/intake/add-folder";
import { CloneFromGitHub } from "./screens/intake/clone";
import { OnboardingReview } from "./screens/intake/onboarding";
import { NewProjectFlow } from "./screens/new-project";
import { ProjectPage } from "./screens/project";
import { Settings } from "./screens/settings";
import { AppShell, Banner, BrandMark, Button, Icon, Sidebar, Skeleton, Toast, navigate, shortcutKey, useRoute, type Route } from "./ui";
import { GoToLayer } from "./ui/go-to";

/** Support details the engine-down screen copies, like the TUI's: what is wrong, the window
 * size and the time, never paths, accounts or secrets. Inputs: the window size and the time.
 * Output: plain text lines. */
export function supportDetails(width: number, height: number, now: Date): string {
  return `CrazyAgent desktop\nEngine: not running\nProblem: engine_unreachable\nWindow: ${Math.round(width)}x${Math.round(height)}\nTime: ${now.toISOString()}\n`;
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
      {copied === "copied" && <p className="secondary" role="status">Copied. Paste it into your message to support.</p>}
      {typeof copied === "object" && <><p className="secondary" role="alert">Couldn't copy. Select the details below and copy them.</p>
        <pre className="support-details">{copied.failed}</pre></>}
    </div>
  </main>;
}

function Launching() {
  return <div className="shell" aria-busy="true">
    <aside className="sidebar"><BrandMark /><Skeleton shape="line" count={6} /></aside>
    <main id="main" className="main"><div className="page"><div className="page-header"><Skeleton shape="line" /></div><Skeleton shape="card" count={6} /></div></main>
  </div>;
}

/** The routed screen inside the shell. Each project page and onboarding review is keyed by its
 * project id, so a switch to another project starts a fresh page and never keeps the last
 * project's master ticket, name, settings or late reads. Inputs: the route and the session.
 * Output: the screen element. */
export function Screen({ route, session }: { route: Route; session: Session }) {
  const projects = session.projects.kind === "ok" ? session.projects.value : undefined;
  switch (route.name) {
    case "settings": return <Settings tab={route.tab} action={route.action} />;
    case "new-project": return <NewProjectFlow step={route.step} />;
    case "add-project": return route.way === "clone" ? <CloneFromGitHub /> : <AddFolder />;
    case "onboarding": return <OnboardingReview key={route.id} id={route.id} />;
    case "project": return <ProjectPage key={route.id} id={route.id} tab={route.tab} name={projects?.find((project) => project.id === route.id)?.name} />;
    default: return <Home />;
  }
}

/** The whole app for one session and route: the engine-down state, the launch skeleton, the
 * first-run Welcome page while the profile is empty, or the shell with the routed screen.
 * Inputs: the session and the current route. */
export function AppView({ session, route }: { session: Session; route: Route }) {
  if (session.engine === "down") return <EngineDown onRetry={session.retry} />;
  if (session.engine === "connecting" || session.profile.kind === "loading") return <Launching />;
  const profile = session.profile.kind === "ok" ? session.profile.value : undefined;
  if (session.profile.kind === "ok" && profile === null || route.name === "welcome") {
    return <SessionContext.Provider value={session}>
      <Welcome onSaved={(_, next) => { void session.refreshProfile().then(() => navigate(next ?? { name: "home" }, true)); }}
        onSkip={(next) => navigate(next ?? { name: "home" }, true)} />
    </SessionContext.Provider>;
  }
  const projects = session.projects.kind === "ok" ? session.projects.value.map((project) => ({ id: project.id, name: project.name })) : undefined;
  return <SessionContext.Provider value={session}>
    <AppShell toast={<Toast message={session.toast} />}
      sidebar={<Sidebar route={route} projects={projects} profileName={profile?.name} engine={session.engine} onRetry={session.retry} />}>
      {session.unconfirmed && <Banner action={<Button size="small" onClick={() => void session.checkUnconfirmed()}>Check again</Button>}>
        We couldn't confirm your last change. Check it and save again.
      </Banner>}
      <Screen route={route} session={session} />
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

/** The desktop app: one engine session, the hash route and the app-wide shortcuts. */
export function App() {
  const session = useSession();
  const route = useRoute();
  useShortcuts();
  return <AppView session={session} route={route} />;
}
