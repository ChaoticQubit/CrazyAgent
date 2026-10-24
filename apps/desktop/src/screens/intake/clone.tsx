import { useEffect, useRef, useState, type RefObject } from "react";
import { cloneRepo, gitHubSignIn, gitHubToken, gitHubUseCli, intakeCopy, loadGitHubRepos, loadGitHubStatus } from "../../api-intake";
import { openLink } from "../../api-projects";
import type { GitHubDeviceCode, GitHubRepo, GitHubStatus } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, Icon, LoadError, NotReady, PageHeader, SearchField, SecretField, Skeleton, navigate } from "../../ui";
import { displayText } from "../conversation";
import { folderLabel } from "../project/overview";
import { GITHUB_POLL_MS, REPO_PAGES_MAX, accountLine, deviceLine, gitHubPage, repoMatches, repoMeta } from "./intake";
import "../project/projects.css";
import "./intake.css";

export interface DeviceFlow {
  code: GitHubDeviceCode;
  started: number;
}

/** GitHub sign-in as a view of its state: Sign in with GitHub, the GitHub CLI or a pasted token;
 * then the device code with how long it works. The token field is uncontrolled and never shown back.
 * Inputs: the device flow or null, now in milliseconds, a busy flag, the last problem, whether the
 * token field is open, its ref and the handlers. Output: the sign-in panel. */
export function GitHubSignInView({ device, now, busy, problem, tokenOpen, tokenRef, onDevice, onCli, onTokenOpen, onToken, onOpenPage }: {
  device: DeviceFlow | null;
  now: number;
  busy: boolean;
  problem: string;
  tokenOpen: boolean;
  tokenRef: RefObject<HTMLInputElement | null>;
  onDevice: () => void;
  onCli: () => void;
  onTokenOpen: () => void;
  onToken: () => void;
  onOpenPage: (url: string) => void;
}) {
  const timing = device ? deviceLine(device.code.expires_s, device.started, now) : null;
  const page = device ? gitHubPage(device.code.verification_uri) : null;
  return <section className="pj-stack-s in-panel" aria-label="Sign in to GitHub">
    <h2>Sign in to GitHub</h2>
    <p className="pj-secondary">CrazyAgent keeps the GitHub sign-in in your Mac's keychain and uses it only to list and clone. Agents never get it; they commit locally and you push.</p>
    {device && timing && !timing.expired && <div className="pj-stack-s">
      <p className="in-code" aria-label="Your GitHub code">{displayText(device.code.user_code)}</p>
      <p role="status">{timing.text}</p>
      <span className="pj-row">
        {page && <Button variant="primary" icon="external-link" onClick={() => onOpenPage(page)}>Open GitHub</Button>}
        <span className="pj-small pj-muted">Waiting for you to approve on GitHub...</span>
      </span>
    </div>}
    {device && timing?.expired && <div className="pj-stack-s"><p role="status">{timing.text}</p>
      <Button variant="primary" icon="refresh-cw" pending={busy} pendingLabel="Starting..." onClick={onDevice}>Start again</Button></div>}
    {!device && <span className="pj-row">
      <Button variant="primary" icon="log-in" pending={busy} pendingLabel="Starting..." onClick={onDevice}>Sign in with GitHub</Button>
      <Button icon="terminal" disabled={busy} onClick={onCli}>Use the GitHub CLI</Button>
      {!tokenOpen && <Button variant="quiet" icon="key-round" disabled={busy} onClick={onTokenOpen}>Paste a token instead</Button>}
    </span>}
    {tokenOpen && !device && <form className="pj-stack-s" onSubmit={(event) => { event.preventDefault(); onToken(); }}>
      <SecretField id="gh-token" label="GitHub token" inputRef={tokenRef} maxLength={255}
        helper="A fine-grained token that can read the repositories you want to clone. It goes to your keychain and isn't shown again." />
      <span><Button type="submit" icon="key-round" disabled={busy}>Save token</Button></span>
    </form>}
    {problem && <p className="pj-error" role="alert">{displayText(problem)}</p>}
  </section>;
}

/** The signed-in account's repositories with a search and Clone on each.
 * Inputs: the GitHub status, the repositories read so far, the search text and its handler, whether
 * more pages exist and the Show more handler, the repository being cloned and the Clone handler.
 * Output: the list. */
export function RepoListView({ status, repos, query, onQuery, more, onMore, cloning, onClone }: {
  status: GitHubStatus;
  repos: Loaded<GitHubRepo[]>;
  query: string;
  onQuery: (query: string) => void;
  more: boolean;
  onMore: () => void;
  cloning: string | null;
  onClone: (repo: GitHubRepo) => void;
}) {
  const shown = repos.kind === "ok" ? repoMatches(repos.value, query) : [];
  return <section className="pj-stack-s" aria-label="Your repositories">
    <p className="pj-secondary">{accountLine(status)}</p>
    {repos.kind === "loading" && <Skeleton shape="row" count={4} />}
    {repos.kind === "not_ready" && <NotReady what="Listing GitHub repositories" compact />}
    {repos.kind === "error" && <p className="pj-error" role="alert">{repos.message}</p>}
    {repos.kind === "ok" && <>
      <SearchField label="Search your repositories" value={query} onValue={onQuery} />
      {repos.value.length === 0 ? <p className="pj-muted">No repositories on this account.</p>
        : shown.length === 0 ? <p className="pj-muted">No repositories match "{query.trim()}".</p>
        : <ul className="in-list">{shown.map((repo) => <li className="in-row" key={repo.full_name}>
          <span className="in-row-main"><span className="pj-row"><Icon name={repo.private ? "lock" : "globe"} size={14} /><strong className="pj-mono">{displayText(repo.full_name)}</strong></span>
            <span className="pj-small pj-secondary">{repoMeta(repo)}</span></span>
          <Button size="small" icon="git-branch" pending={cloning === repo.full_name} pendingLabel="Cloning..." disabled={cloning !== null && cloning !== repo.full_name}
            onClick={() => onClone(repo)}>Clone</Button>
        </li>)}</ul>}
      {more && <span><Button variant="quiet" onClick={onMore}>Show more</Button></span>}
    </>}
  </section>;
}

/** Sign in to GitHub with its own state: the device flow polled every few seconds while the code
 * is live, the GitHub CLI, or a pasted token read once from the field and cleared.
 * Inputs: the status shown and the status reload. Output: the sign-in panel. */
export function GitHubSignIn({ status, reloadStatus }: { status: GitHubStatus; reloadStatus: () => void }) {
  const [device, setDevice] = useState<DeviceFlow | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [tokenOpen, setTokenOpen] = useState(false);
  const tokenRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!device || status.signed_in) return;
    const timer = window.setInterval(() => { setNow(Date.now()); reloadStatus(); }, GITHUB_POLL_MS);
    return () => window.clearInterval(timer);
  }, [device, status.signed_in]);

  const failed = (result: { kind: "not_ready" } | { kind: "error"; code: string }, what: string) =>
    setProblem(result.kind === "not_ready" ? `${what} isn't in this build.` : intakeCopy(result.code));

  const startDevice = async () => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const result = await gitHubSignIn();
    setBusy(false);
    if (result.kind !== "ok") { failed(result, "Sign in with GitHub"); return; }
    setNow(Date.now());
    setDevice({ code: result.value, started: Date.now() });
  };

  const signedInWith = async (action: () => ReturnType<typeof gitHubUseCli>, what: string) => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const result = await action();
    setBusy(false);
    if (result.kind !== "ok") { failed(result, what); return; }
    if (!result.value.signed_in) setProblem(result.value.problem ?? "GitHub didn't sign you in. Try another way.");
    reloadStatus();
  };

  const sendToken = () => {
    const field = tokenRef.current;
    if (!field) return;
    const token = field.value;
    field.value = "";
    void signedInWith(() => gitHubToken(token), "Saving a GitHub token");
  };

  return <GitHubSignInView device={device} now={now} busy={busy} problem={problem || (status.problem ?? "")}
    tokenOpen={tokenOpen} tokenRef={tokenRef} onDevice={() => void startDevice()} onCli={() => void signedInWith(gitHubUseCli, "Using the GitHub CLI")}
    onTokenOpen={() => setTokenOpen(true)} onToken={sendToken} onOpenPage={(url) => void openLink(url)} />;
}

/** Clone from GitHub: signs in through the device flow, the GitHub CLI or a token, lists the
 * account's repositories page by page and clones one into the projects folder. */
export function CloneFromGitHub() {
  const session = useAppSession();
  const [status, reloadStatus] = useLoad(loadGitHubStatus);
  const [problem, setProblem] = useState("");
  const [repos, setRepos] = useState<Loaded<GitHubRepo[]>>({ kind: "loading" });
  const [next, setNext] = useState<number | null>(null);
  const [pages, setPages] = useState(0);
  const [query, setQuery] = useState("");
  const [cloning, setCloning] = useState<string | null>(null);
  const signedIn = status.kind === "ok" && status.value.signed_in;

  useEffect(() => {
    if (!signedIn) return;
    let live = true;
    void loadGitHubRepos(1).then((result) => {
      if (!live) return;
      setRepos(result.kind === "ok" ? { kind: "ok", value: result.value.repos } : result);
      setNext(result.kind === "ok" ? result.value.next : null);
      setPages(1);
    });
    return () => { live = false; };
  }, [signedIn]);

  const failed = (result: { kind: "not_ready" } | { kind: "error"; code: string }, what: string) =>
    setProblem(result.kind === "not_ready" ? `${what} isn't in this build.` : intakeCopy(result.code));

  const more = async () => {
    if (next === null) return;
    const result = await loadGitHubRepos(next);
    if (result.kind !== "ok") { failed(result, "Listing more repositories"); return; }
    setRepos((old) => old.kind === "ok" ? { kind: "ok", value: [...old.value, ...result.value.repos] } : old);
    setNext(result.value.next);
    setPages((old) => old + 1);
  };

  const clone = async (repo: GitHubRepo) => {
    if (cloning) return;
    setCloning(repo.full_name);
    setProblem("");
    const result = await cloneRepo(repo.full_name);
    setCloning(null);
    if (result.kind !== "ok") { failed(result, "Cloning"); return; }
    session.showToast(`${result.value.full_name} is in your projects folder at ${folderLabel(result.value.path)}. Set it up from Home.`);
    navigate({ name: "home" });
  };

  return <div className="pj in-page">
    <PageHeader title="Clone from GitHub" description="Pick a repository and CrazyAgent clones it into your projects folder." />
    {status.kind === "loading" && <Skeleton shape="line" count={3} />}
    {status.kind === "not_ready" && <NotReady what="Cloning from GitHub" />}
    {status.kind === "error" && <LoadError message={status.message} onRetry={reloadStatus} />}
    {status.kind === "ok" && !status.value.signed_in && <GitHubSignIn status={status.value} reloadStatus={reloadStatus} />}
    {status.kind === "ok" && status.value.signed_in && <>
      <RepoListView status={status.value} repos={repos} query={query} onQuery={setQuery} more={next !== null && pages < REPO_PAGES_MAX}
        onMore={() => void more()} cloning={cloning} onClone={(repo) => void clone(repo)} />
      {problem && <p className="pj-error" role="alert">{problem}</p>}
    </>}
  </div>;
}
