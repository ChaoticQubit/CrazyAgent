import { useState, type ReactNode } from "react";
import { gitHubSignOut, intakeCopy, loadGitHubStatus } from "../../api-intake";
import type { GitHubStatus } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, LoadError, NotReady, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { GitHubSignIn } from "../intake/clone";
import { accountLine } from "../intake/intake";
import "../project/projects.css";
import "../intake/intake.css";

/** Settings, GitHub while signed in: who CrazyAgent is signed in as, how, and Sign out.
 * Inputs: the status, a busy flag, the last problem and the Sign out handler. Output: the panel. */
export function GitHubAccountView({ status, busy, problem, onSignOut }: {
  status: GitHubStatus;
  busy: boolean;
  problem: string;
  onSignOut: () => void;
}) {
  return <section className="pj-stack-s in-panel" aria-label="GitHub account">
    <h2>GitHub account</h2>
    <p>{accountLine(status)}</p>
    <p className="pj-secondary">{status.method === "cli"
      ? "CrazyAgent uses the GitHub CLI's sign-in on this Mac to list and clone. Signing out here leaves the GitHub CLI signed in."
      : "CrazyAgent keeps this sign-in in your Mac's keychain and uses it only to list and clone. Agents never get it."}</p>
    {status.problem && <p className="pj-error" role="alert">{displayText(status.problem)}</p>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <span><Button variant="danger-text" icon="log-out" pending={busy} pendingLabel="Signing out..." onClick={onSignOut}>Sign out of GitHub</Button></span>
  </section>;
}

/** Settings, GitHub as a view of its reads: the account and Sign out while signed in, otherwise
 * the sign-in choices. Inputs: the status read, the sign-in panel, the account panel and retry. */
export function GitHubSettingsView({ status, signIn, account, onRetry }: {
  status: Loaded<GitHubStatus>;
  signIn: (status: GitHubStatus) => ReactNode;
  account: (status: GitHubStatus) => ReactNode;
  onRetry: () => void;
}) {
  if (status.kind === "loading") return <Skeleton shape="line" count={3} />;
  if (status.kind === "not_ready") return <NotReady what="Signing in to GitHub" />;
  if (status.kind === "error") return <LoadError message={status.message} onRetry={onRetry} />;
  return <div className="pj pj-stack">
    <p className="pj-secondary">GitHub lets you clone your repositories into the projects folder. Agents commit locally; you push.</p>
    {status.value.signed_in ? account(status.value) : signIn(status.value)}
  </div>;
}

/** Settings, GitHub: reads the status, signs in through the device flow, the GitHub CLI or a
 * token, and signs out after a confirm. */
export function GitHubSettings() {
  const session = useAppSession();
  const [status, reload] = useLoad(loadGitHubStatus);
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");

  const signOut = async () => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const result = await gitHubSignOut();
    setBusy(false);
    setAsking(false);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Signing out of GitHub isn't in this build." : intakeCopy(result.code)); return; }
    if (result.value.signed_in) { setProblem("GitHub is still signed in. Try again."); reload(); return; }
    reload();
    session.showToast("Signed out of GitHub.");
  };

  return <>
    <GitHubSettingsView status={status} onRetry={reload}
      signIn={(value) => <GitHubSignIn status={value} reloadStatus={reload} />}
      account={(value) => <GitHubAccountView status={value} busy={busy} problem={problem} onSignOut={() => setAsking(true)} />} />
    <ConfirmDialog open={asking} title="Sign out of GitHub?" confirmLabel="Sign out" pending={busy}
      onCancel={() => setAsking(false)} onConfirm={() => void signOut()}>
      CrazyAgent deletes its GitHub sign-in from your keychain. Projects you cloned stay where they are; cloning again needs a new sign-in.
    </ConfirmDialog>
  </>;
}
