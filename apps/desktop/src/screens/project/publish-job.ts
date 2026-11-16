import { intakeCopy } from "../../api-intake";
import type { Result } from "../../api";
import type { GitHubPublishJob, GitHubPublishLeft } from "../../engine-client";

/**
 * The words a person reads for each rule of the standard secure setup, keyed by the
 * engine's rule name. The TUI shows the same text (DEC-182).
 */
export const RULE_LABELS: Readonly<Record<string, string>> = {
  squash_only: "Squash merge only",
  delete_head_branches: "Delete branches after merge",
  issues_on: "Issues on",
  wiki_off: "Wiki off",
  projects_off: "Projects off",
  auto_merge_off: "Auto-merge off",
  pull_request: "Changes go through pull requests",
  conversation_resolution: "Resolve conversations before merging",
  no_force_push: "No force pushes to main",
  no_deletion: "Main can't be deleted",
  linear_history: "Linear history",
  required_checks: "Required checks pass before merging",
  signed_commits: "Signed commits",
  tag_protection: "Release tags protected",
  dependabot_alerts: "Dependabot alerts",
  dependabot_security_updates: "Dependabot security updates",
  secret_scanning: "Secret scanning",
  push_protection: "Push protection for secrets",
  private_vulnerability_reporting: "Private vulnerability reporting",
  actions_verified_only: "Actions from GitHub and verified creators only",
  actions_read_only_token: "Read-only token for Actions",
  actions_outside_pr_approval: "Approval for outside contributors' workflows",
  codeql_default_setup: "CodeQL code scanning",
  dependabot_version_updates: "Dependabot version updates",
  security_policy: "Security policy (SECURITY.md)",
};

export const DRAFT_POLL_MS = 2000;
export const DRAFT_AGENT = "The project's agent drafted these lines from file names only. Check them before you approve.";
export const DRAFT_STOPPED = "The draft stopped, so CrazyAgent's standard lines are shown.";
export const DRAFT_BASELINE = "No agent drafted these lines, so CrazyAgent's standard lines are shown.";
export const PART_WAY = "Publishing stopped part way. Try again to go on from there.";

const FINDING_KINDS: Readonly<Record<string, string>> = {
  token: "looks like an access token",
  private_key: "looks like a private key",
  named_value: "looks like a secret value",
};

/** Where a publish stands, as the Overview card shows it: nothing, the draft running, the
 * lines to approve with who drafted them, the secrets that block it, a stop with its
 * reason, or the finished repository with the rules it could not set. */
export type PublishView =
  | { kind: "none" }
  | { kind: "drafting" }
  | { kind: "lines"; lines: string[]; note: string }
  | { kind: "blocked"; findings: string[]; paths: string[]; truncated: boolean }
  | { kind: "stopped"; reason: string }
  | { kind: "done"; line: string; missed: string[] };

/** Turns a publish job into what the Overview card shows. A finding names its file, line
 * and kind only, never the value. Input: the job, or null. Output: the view. */
export function publishView(job: GitHubPublishJob | null): PublishView {
  if (!job) return { kind: "none" };
  const step = job.step;
  switch (step.step) {
    case "drafting":
      return { kind: "drafting" };
    case "lines":
      return { kind: "lines", lines: step.lines, note: step.draft.by === "agent" ? DRAFT_AGENT : step.draft.reason === "github_publish_draft_stopped" ? DRAFT_STOPPED : DRAFT_BASELINE };
    case "blocked":
      return {
        kind: "blocked",
        findings: step.scan.findings.map((finding) => `${finding.path}, line ${finding.line}: ${FINDING_KINDS[finding.kind] ?? FINDING_KINDS.named_value}`),
        paths: [...new Set(step.scan.findings.map((finding) => finding.path))],
        truncated: step.scan.truncated,
      };
    case "done": {
      const wanted = step.outcomes.filter((outcome) => outcome.on);
      const set = wanted.filter((outcome) => outcome.state.state === "applied").length;
      return {
        kind: "done",
        line: `Published as ${step.full_name}. ${set} of ${wanted.length} rules set.`,
        missed: wanted.filter((outcome) => outcome.state.state !== "applied").map((outcome) => RULE_LABELS[outcome.rule] ?? outcome.rule),
      };
    }
    default:
      return { kind: "stopped", reason: job.stopped ? intakeCopy(job.stopped) : PART_WAY };
  }
}

/** Says what a discarded publish left on GitHub. Input: the discard's answer. Output: the
 * sentence, or "" when it left nothing. */
export function leftLine(left: GitHubPublishLeft | null): string {
  if (!left) return "";
  return left.left === "made"
    ? `The empty repository ${left.full_name} stays on GitHub. Delete it there if you don't want it.`
    : `GitHub may have made ${left.full_name}. Check GitHub and delete it if you don't want it.`;
}

/** Tells whether Discard is offered: any publish before its push, since a pushed one keeps
 * its code on GitHub and goes on with Try again. Input: the job, or null. Output: true when
 * Discard is offered. */
export function discardable(job: GitHubPublishJob | null): boolean {
  return job !== null && job.step.step !== "pushed" && job.step.step !== "done";
}

export const PROMPT_LINE = "This project is not on GitHub. Publish it?";
export const PROMPT_PUBLISH = "Publish";
export const PROMPT_NOT_NOW = "Not now";
export const PROMPT_NEVER = "Don't ask again";

/** Whether the Overview asks to publish the project on GitHub (DEC-182 S5b): its publish read
 * found no job and the engine says to ask. Inputs: the publish read and the prompt read, each
 * null until it answers. Output: true to show the notice. */
export function noticeShown(read: Result<GitHubPublishJob | null> | null, prompt: Result<boolean> | null): boolean {
  return read?.kind === "ok" && read.value === null && prompt?.kind === "ok" && prompt.value;
}

export const ENGINE_STARTING = "CrazyAgent's engine is still starting. Try again in a moment.";

/** What the Overview card keeps after a publish read (rv-ds241 S5a F1): a read that answered
 * replaces the last one and clears the read's problem, and a failed one keeps the last job
 * shown, so its draft poll goes on, and gives the problem the card shows with Try again.
 * Inputs: the read kept so far, null before any answered, and the new read. Output: the read
 * to keep and the problem, null when the read answered. */
export function readKept(kept: Result<GitHubPublishJob | null> | null, result: Result<GitHubPublishJob | null>): { read: Result<GitHubPublishJob | null> | null; failed: string | null } {
  if (result.kind === "ok") return { read: result, failed: null };
  return { read: kept, failed: result.kind === "error" ? result.message : ENGINE_STARTING };
}
