import { useRef, useState, type ReactNode } from "react";
import { gitHubSignOut, intakeCopy, loadGitHubRuleDefaults, loadGitHubStatus, saveGitHubRuleDefaults } from "../../api-intake";
import type { GitHubStatus } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, LoadError, NotReady, SettingLine, SettingSection, Skeleton, Switch } from "../../ui";
import { DEFAULTS_ABOUT, DEFAULTS_TITLE, STANDARD_LABEL, STANDARD_RULES, withRule } from "../project/github-rules";
import { RULE_LABELS } from "../project/publish-job";
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
  return <SettingSection title="GitHub account">
    <SettingLine label={accountLine(status)} help={status.method === "cli"
      ? "CrazyAgent uses the GitHub CLI's sign-in on this Mac to list, clone and publish your repositories and set their rules. Signing out here leaves the GitHub CLI signed in."
      : "CrazyAgent keeps this sign-in in your Mac's keychain and uses it to list, clone and publish your repositories and set their rules. Agents never get it."}>
      <Button variant="danger-text" icon="log-out" pending={busy} pendingLabel="Signing out..." onClick={onSignOut}>Sign out of GitHub</Button>
    </SettingLine>
    {status.problem && <p className="set-body pj-error" role="alert">{displayText(status.problem)}</p>}
    {problem && <p className="set-body pj-error" role="alert">{problem}</p>}
  </SettingSection>;
}

/** Settings, GitHub, New repositories as a view of its read: one switch per rule, each
 * marked when it differs from the standard setup, and Use the standard setup. Inputs: the
 * read, the last save's problem and the handlers. Output: the section. */
export function GitHubDefaultsView({ read, problem, onRule, onStandard, onRetry }: {
  read: Loaded<string[]>;
  problem: string;
  onRule: (rule: string, on: boolean) => void;
  onStandard: () => void;
  onRetry: () => void;
}) {
  return <SettingSection title={DEFAULTS_TITLE} about={DEFAULTS_ABOUT} actions={<Button variant="secondary" onClick={onStandard}>{STANDARD_LABEL}</Button>}>
    {read.kind === "loading" && <Skeleton shape="line" count={3} />}
    {read.kind === "not_ready" && <NotReady what="Defaults for new repositories" />}
    {read.kind === "error" && <>
      <p className="set-body pj-error" role="alert">{read.message}</p>
      <Button variant="secondary" icon="refresh-cw" onClick={onRetry}>Try again</Button>
    </>}
    {read.kind === "ok" && Object.entries(RULE_LABELS).map(([rule, label]) => {
      const on = read.value.includes(rule);
      return <SettingLine key={rule} label={label} changed={on !== STANDARD_RULES.includes(rule)}>
        <Switch id={`github-default-${rule}`} label={label} hideLabel checked={on} onChecked={(next) => onRule(rule, next)} />
      </SettingLine>;
    })}
    {problem && <p className="set-body pj-error" role="alert">{problem}</p>}
  </SettingSection>;
}

/** Settings, GitHub, New repositories: reads the saved defaults and saves the whole set on
 * each switch or on Use the standard setup, then shows what the engine read back, which is
 * another client's set when it saved in between; a failed save reads the saved set again.
 * Output: the section. */
function GitHubDefaultsSection() {
  const [read, reload] = useLoad(loadGitHubRuleDefaults);
  const [saved, setSaved] = useState<string[] | null>(null);
  const [problem, setProblem] = useState("");
  const saving = useRef(false);
  const shown: Loaded<string[]> = saved === null ? read : { kind: "ok", value: saved };

  const save = async (rules: string[]) => {
    if (saving.current) return;
    saving.current = true;
    setProblem("");
    const result = await saveGitHubRuleDefaults(rules);
    saving.current = false;
    if (result.kind === "ok") { setSaved(result.value); return; }
    setProblem(result.kind === "not_ready" ? "Saving defaults for new repositories isn't in this build." : result.message);
    setSaved(null);
    reload();
  };

  return <GitHubDefaultsView read={shown} problem={problem}
    onRule={(rule, on) => { if (shown.kind === "ok") void save(withRule(shown.value, rule, on)); }}
    onStandard={() => void save([...STANDARD_RULES])}
    onRetry={() => { setSaved(null); reload(); }} />;
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
  return <div className="pj pj-stack">{status.value.signed_in ? account(status.value) : signIn(status.value)}</div>;
}

/** Settings, GitHub: reads the status, signs in through the device flow, the GitHub CLI or a
 * token, and signs out after a confirm. */
export function GitHubSettings() {
  const session = useAppSession();
  const [status, reload] = useLoad(loadGitHubStatus, [], "github-status");
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
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Signing out of GitHub isn't in this build." : intakeCopy(result)); return; }
    if (result.value.signed_in) { setProblem("GitHub is still signed in. Try again."); reload(); return; }
    reload();
    session.showToast("Signed out of GitHub.");
  };

  return <div className="stack">
    <div className="settings-top"><div><h2>GitHub</h2><p>GitHub lets you clone your repositories and publish new projects with a standard secure setup. Agents commit locally and never get your sign-in.</p></div></div>
    <GitHubSettingsView status={status} onRetry={reload}
      signIn={(value) => <GitHubSignIn status={value} reloadStatus={reload} />}
      account={(value) => <>
        <GitHubAccountView status={value} busy={busy} problem={problem} onSignOut={() => setAsking(true)} />
        <GitHubDefaultsSection />
      </>} />
    <ConfirmDialog open={asking} title="Sign out of GitHub?" confirmLabel="Sign out" pending={busy}
      onCancel={() => setAsking(false)} onConfirm={() => void signOut()}>
      CrazyAgent deletes its GitHub sign-in from your keychain. Projects you cloned or published stay where they are; cloning, publishing and changing GitHub rules need a new sign-in.
    </ConfirmDialog>
  </div>;
}
