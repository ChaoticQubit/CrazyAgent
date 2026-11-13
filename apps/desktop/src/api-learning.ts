import { call, nextGeneration, receiptResult, type Result } from "./api";
import type { IntentReceipt } from "./engine-client";

/** The four groups Home lists the learning steps under, the engine's `learning::Topic`. */
export type LearningTopic = "basics" | "core_loop" | "ide_and_terminals" | "process_metrics_usage";

/** One engine learning step, the engine's `learning::StepId`. */
export type LearningStepId = "project" | "master" | "agents" | "question" | "change_set" | "ide" | "terminal" | "process" | "metrics" | "usage";

/** A mark the app sets where no other engine row tells the step, the engine's `learning::MarkStep`. */
export type LearningMark = "terminal" | "process" | "metrics" | "usage" | "skipped";

/** One learning step with the words both clients show and how many rows or marks show it done. */
export interface LearningStep {
  id: LearningStepId;
  topic: LearningTopic;
  title: string;
  body: string;
  count: number;
}

/** The learning steps in order, and whether the owner skipped the guide. */
export interface LearningProgress {
  steps: LearningStep[];
  skipped: boolean;
}

/** Reads the Learn CrazyAgent steps. Output: the steps and the skip, or a plain error. */
export function loadLearningProgress(): Promise<Result<LearningProgress>> {
  return call<LearningProgress>("desktop_learning_progress");
}

/** Sets or clears one learning mark: a page opened, or the guide skipped or shown again.
 * Inputs: the mark and whether it is set. Output: ok once it stands, also when the engine
 * already held it and nothing was sent, else the plain refusal. */
export async function setLearningMark(step: LearningMark, on: boolean): Promise<Result<void>> {
  const result = await call<IntentReceipt | null>("desktop_set_learning_mark", { step, on, generation: nextGeneration() });
  if (result.kind !== "ok") return result;
  return result.value === null ? { kind: "ok", value: undefined } : receiptResult(result.value);
}
