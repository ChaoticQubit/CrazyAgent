import type { GitHubRuleOutcome, GitHubRuleState } from "../../engine-client";
import { displayText } from "../conversation";
import { RULE_LABELS } from "./publish-job";

/** What one rule's switch shows on Project > Settings > GitHub. */
export interface RuleSwitch {
  label: string;
  checked: boolean;
  disabledReason: string | null;
  problem: string | null;
  help: string | null;
}

const ORDER = Object.keys(RULE_LABELS);
const MESSAGE_SHOWN_MAX = 300;

/** The rules DEC-182's standard secure setup turns on, in the engine's order. */
export const STANDARD_RULES: readonly string[] = ORDER.filter((rule) => rule !== "signed_commits");

export const RULES_NOT_PUBLISHED = "GitHub rules show here for a project CrazyAgent published on GitHub, once its publish is done.";
export const DEFAULTS_TITLE = "New repositories";
export const DEFAULTS_ABOUT = "The rules each project CrazyAgent publishes on GitHub starts with. A publish already started keeps its own copy.";
export const STANDARD_LABEL = "Use the standard setup";

/** The rules on after one switch changes, in the engine's order; names the engine does not
 * know are left out. Inputs: the rules on, the rule and whether it is now on. Output: the list. */
export function withRule(rules: readonly string[], rule: string, on: boolean): string[] {
  const wanted = new Set(rules);
  if (on) wanted.add(rule);
  else wanted.delete(rule);
  return ORDER.filter((name) => wanted.has(name));
}

/** One plain sentence for a rule's live state, with GitHub's own words bounded and their
 * hidden characters shown. Input: the state. Output: the sentence, or null while applied. */
export function ruleStateLine(state: GitHubRuleState): string | null {
  if (state.state === "applied") return null;
  if (state.state === "refused") {
    const message = displayText(Array.from(state.message).slice(0, MESSAGE_SHOWN_MAX).join(""));
    return state.status === 0 ? `Not sent: ${message}` : `GitHub refused this (HTTP ${state.status}): ${message}`;
  }
  const reason = displayText(state.reason);
  return `${reason.charAt(0).toUpperCase()}${reason.slice(1)}.`;
}

/** What one rule's switch shows: a rule that does not apply, or waits, cannot change; a
 * refused one can be tried again. Inputs: the rule's outcome and the check names the first
 * CI run reported. Output: its label, whether it is on, why it cannot change, and its notes. */
export function ruleSwitch(outcome: GitHubRuleOutcome, checks: readonly string[]): RuleSwitch {
  const line = ruleStateLine(outcome.state);
  const fixed = outcome.state.state === "not_applicable" || outcome.state.state === "waiting";
  const named = outcome.rule === "required_checks" && checks.length > 0;
  return {
    label: RULE_LABELS[outcome.rule] ?? displayText(outcome.rule),
    checked: outcome.on,
    disabledReason: fixed ? line : null,
    problem: outcome.state.state === "refused" ? line : null,
    help: named ? `Checks the first CI run named: ${checks.map(displayText).join(", ")}.` : null,
  };
}
