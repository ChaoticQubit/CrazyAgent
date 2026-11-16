import { useEffect, useRef, useState } from "react";
import type { Result } from "../../api";
import { loadGitHubRules, setGitHubRule } from "../../api-intake";
import type { GitHubRepoRules } from "../../engine-client";
import { LoadError, Switch } from "../../ui";
import { RULES_NOT_PUBLISHED, ruleSwitch } from "./github-rules";
import { RULE_LABELS } from "./publish-job";
import { NotReady } from "./shared";
import type { ProjectData } from "./shared";

const SENDING_SWITCH = "Sending to GitHub...";
const RULES_NOT_READY = "Changing GitHub rules isn't in this build.";

/** Sends one switch for the repository the page showed and answers the page to show next:
 * the page the change answers, or, when the change fails, its problem and the page read
 * again, since the change may have landed on GitHub before the failure. Inputs: the project
 * id, the `owner/name` shown, the rule, whether it is on, and the change and read calls.
 * Output: the page and the change's problem. */
export async function sendRule(
  project: string,
  repo: string,
  rule: string,
  on: boolean,
  send: typeof setGitHubRule = setGitHubRule,
  read: typeof loadGitHubRules = loadGitHubRules,
): Promise<{ page: Result<GitHubRepoRules>; problem: string }> {
  const changed = await send(project, repo, rule, on);
  if (changed.kind === "ok") return { page: changed, problem: "" };
  const problem = changed.kind === "not_ready" ? RULES_NOT_READY : changed.message;
  return { page: await read(project), problem };
}

/** Project > Settings > GitHub as a view of its read: the repository, then one switch per
 * rule with its live state, GitHub's refusal on its own rule, and the rule being sent.
 * Inputs: the read, the rule being sent, the last change's problem, the switch handler and
 * retry. Output: the section. */
export function GitHubRulesView({ read, busy, problem, onRule, onRetry }: {
  read: Result<GitHubRepoRules> | undefined;
  busy: string | null;
  problem: string;
  onRule: (rule: string, on: boolean) => void;
  onRetry: () => void;
}) {
  const page = read?.kind === "ok" ? read.value : null;
  return <div className="pj-stack">
    <h2>GitHub</h2>
    {read === undefined && <p className="pj-muted" role="status">Loading...</p>}
    {read?.kind === "not_ready" && <NotReady what="GitHub rules aren't in this build." />}
    {read?.kind === "error" && (read.code === "github_rules_not_published"
      ? <p className="pj-secondary">{RULES_NOT_PUBLISHED}</p>
      : <LoadError message={read.message} onRetry={onRetry} />)}
    {page && <>
      <p className="pj-secondary">{`${page.full_name}, ${page.private ? "Private" : "Public"}. Each switch changes the repository on GitHub at once, with your sign-in.`}</p>
      {busy !== null && <p className="pj-note" role="status">{`Sending ${RULE_LABELS[busy] ?? busy} to GitHub. This can take a few minutes.`}</p>}
      {page.rules.map((outcome) => {
        const shown = ruleSwitch(outcome, page.checks);
        return <div className="pj-field" key={outcome.rule}>
          <Switch id={`github-rule-${outcome.rule}`} label={shown.label} checked={shown.checked}
            disabledReason={busy === outcome.rule ? SENDING_SWITCH : shown.disabledReason ?? undefined}
            onChecked={(on) => onRule(outcome.rule, on)} />
          {shown.help && <p className="pj-help">{shown.help}</p>}
          {shown.problem && <p className="pj-error" role="alert">{shown.problem}</p>}
        </div>;
      })}
    </>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
  </div>;
}

/** Project > Settings > GitHub: reads the repository's live rules when it opens, which sends
 * nothing to GitHub that changes it, and sends one switch at a time, showing the page the
 * change answers. Input: the project data. Output: the section. */
export function GitHubRulesSection({ data }: { data: Pick<ProjectData, "id"> }) {
  const [read, setRead] = useState<Result<GitHubRepoRules> | undefined>(undefined);
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState("");
  const current = useRef(data.id);
  const sending = useRef(false);
  current.current = data.id;

  useEffect(() => {
    let live = true;
    sending.current = false;
    setRead(undefined);
    setBusy(null);
    setProblem("");
    void loadGitHubRules(data.id).then((result) => { if (live) setRead(result); });
    return () => { live = false; };
  }, [data.id]);

  async function retry() {
    const id = data.id;
    setRead(undefined);
    const result = await loadGitHubRules(id);
    if (current.current === id) setRead(result);
  }

  async function change(rule: string, on: boolean) {
    if (sending.current || read?.kind !== "ok") return;
    const id = data.id;
    const repo = read.value.full_name;
    sending.current = true;
    setBusy(rule);
    setProblem("");
    const answer = await sendRule(id, repo, rule, on);
    if (current.current !== id) return;
    sending.current = false;
    setBusy(null);
    setRead(answer.page);
    setProblem(answer.problem);
  }

  return <GitHubRulesView read={read} busy={busy} problem={problem} onRule={(rule, on) => void change(rule, on)} onRetry={() => void retry()} />;
}
