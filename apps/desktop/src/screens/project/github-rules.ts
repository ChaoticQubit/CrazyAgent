import type { GitHubRuleOutcome, GitHubRuleState } from "../../engine-client";

/** What one rule's switch shows on Project > Settings > GitHub. */
export interface RuleSwitch {
  label: string;
  checked: boolean;
  disabledReason: string | null;
  problem: string | null;
  help: string | null;
}

/** The rules DEC-182's standard secure setup turns on, in the engine's order. */
export const STANDARD_RULES: readonly string[] = [];

export const RULES_NOT_PUBLISHED = "";
export const DEFAULTS_TITLE = "";
export const DEFAULTS_ABOUT = "";
export const STANDARD_LABEL = "";

/** The rules on after one switch changes, in the engine's order. Inputs: the rules on, the
 * rule and whether it is now on. Output: the new list. */
export function withRule(_rules: readonly string[], _rule: string, _on: boolean): string[] {
  return [];
}

/** One plain sentence for a rule's live state. Input: the state. Output: the sentence, or
 * null while the rule is applied. */
export function ruleStateLine(_state: GitHubRuleState): string | null {
  return null;
}

/** What one rule's switch shows. Inputs: the rule's outcome and the check names the first CI
 * run reported. Output: its label, whether it is on, why it cannot change, and its notes. */
export function ruleSwitch(_outcome: GitHubRuleOutcome, _checks: readonly string[]): RuleSwitch {
  return { label: "", checked: false, disabledReason: null, problem: null, help: null };
}
