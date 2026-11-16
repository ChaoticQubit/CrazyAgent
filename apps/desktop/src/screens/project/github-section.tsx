import type { Result } from "../../api";
import type { GitHubRepoRules } from "../../engine-client";
import type { ProjectData } from "./shared";

/** Project > Settings > GitHub as a view of its read. Inputs: the read, the rule being sent,
 * the last change's problem, the switch handler and retry. Output: the section. */
export function GitHubRulesView(_props: {
  read: Result<GitHubRepoRules> | undefined;
  busy: string | null;
  problem: string;
  onRule: (rule: string, on: boolean) => void;
  onRetry: () => void;
}) {
  return null;
}

/** Project > Settings > GitHub. Input: the project data. Output: the section. */
export function GitHubRulesSection(_props: { data: Pick<ProjectData, "id"> }) {
  return null;
}
