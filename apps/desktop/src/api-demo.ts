import { call, type Result } from "./api";
import type { IntentReceipt } from "./engine-client";

/** Which demo steps the engine saw happen, each from existing rows of the demo project. */
export interface DemoSteps {
  created: boolean;
  goal_sent: boolean;
  agent_started: boolean;
  question_answered: boolean;
  change_set_accepted: boolean;
}

/** One pool as the demo sees it: null when the demo can run on it, else the engine's code
 * for why not, the same check Create the demo refuses with. */
export interface DemoPool {
  pool: string;
  unavailable: string | null;
}

/** The engine's demo read (DEC-153): the demo project while it exists (null once deleted or
 * removed), its name, when it was made, the goal the owner sends the master, the steps seen
 * and every pool with whether the demo can run on it. */
export interface DemoView {
  project_id: string | null;
  name: string;
  created_ms: number | null;
  goal: string;
  steps: DemoSteps;
  pools: DemoPool[];
}

/** Reads the demo. Output: the engine's demo view, or the read's failure. */
export function loadDemo(): Promise<Result<DemoView>> {
  return call<DemoView>("desktop_project_demo");
}

/** Asks the engine to make the demo project on the pool the owner picked. Inputs: the pool id
 * and the intent generation. Output: the receipt, whose project id is the demo's. */
export function createDemo(pool: string, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_create_demo", { pool, generation });
}
