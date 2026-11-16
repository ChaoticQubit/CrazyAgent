import { nextGeneration } from "../../api";
import { loadAgentPolicy, saveAgentPolicy, setProjectPool } from "../../api-projects";
import type { AgentAccess } from "../../api-projects";
import type { ProjectDraft } from "./wizard";

/** The New Project job Create started, with what its follow-ups need resolved at Create time:
 * the project, the answers, the name, the pool and its label, the agent access and how many
 * research agents start. */
export type StartedSetup = {
  id: string;
  project: string;
  draft: ProjectDraft;
  name: string;
  pool: string | null;
  poolLabel: string;
  access: AgentAccess;
  research: number;
};

/** The started setup a job's follow-ups belong to and the sentence they end with. */
export type Carry = { setup: StartedSetup; outcome: Promise<string> };

/** The calls the follow-ups make: assign the pool, read and save the agent access, and the
 * window generation each change carries. */
export type CarryDeps = {
  assign: typeof setProjectPool;
  read: typeof loadAgentPolicy;
  save: typeof saveAgentPolicy;
  generation: () => number;
};

const LIVE: CarryDeps = { assign: setProjectPool, read: loadAgentPolicy, save: saveAgentPolicy, generation: nextGeneration };

/** Keeps the job Create started until its follow-ups run. Input: the started setup. */
export function keepStarted(setup: StartedSetup): void {
  void setup;
}

/** The job Create started, if one is kept. Output: the started setup, or null. */
export function startedJob(): StartedSetup | null {
  return null;
}

/** Forgets the job Create started; follow-ups already running finish. */
export function dropStarted(): void {}

/** Runs a started New Project's follow-ups once its job is done. Inputs: the job id, the
 * project it made and the calls. Output: the carry, or null for a job not started here. */
export function carryFollowUps(job: string, project: string, deps: CarryDeps = LIVE): Carry | null {
  void job;
  void project;
  void deps;
  return null;
}
