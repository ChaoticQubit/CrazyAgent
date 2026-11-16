import { nextGeneration } from "../../api";
import { failureCopy, loadAgentPolicy, receiptState, saveAgentPolicy, setProjectPool } from "../../api-projects";
import type { AgentAccess } from "../../api-projects";
import { noteSetupCarried } from "../intake/intake";
import { accessLabel, policyOf } from "../project/access";
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

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

let slot: { setup: StartedSetup; outcome: Promise<string> | null } | null = null;

/** Keeps the job Create started until its follow-ups run, in place of any kept before; the
 * engine runs one New Project job at a time. Input: the started setup. */
export function keepStarted(setup: StartedSetup): void {
  assert(setup.id !== "" && setup.project !== "" && setup.name !== "", "a started setup names its job and project");
  slot = { setup, outcome: null };
}

/** The job Create started, if one is kept. Output: the started setup, or null. */
export function startedJob(): StartedSetup | null {
  return slot?.setup ?? null;
}

/** Forgets the job Create started; follow-ups already running finish. */
export function dropStarted(): void {
  slot = null;
}

/** Runs a started New Project's follow-ups once its job is done (DEC-177 add.2 Q1 B), whether
 * or not its page is open: the pool, then the agent access. The first caller starts them and
 * every later caller shares that run, all on the project the kept job makes and no other.
 * Inputs: the job id, the project it made and the calls. Output: the carry, or null for a job
 * not started here. */
export function carryFollowUps(job: string, project: string, deps: CarryDeps = LIVE): Carry | null {
  if (slot === null || slot.setup.id !== job) return null;
  assert(project === slot.setup.project, `the follow-ups apply only to the project the kept job makes, ${slot.setup.project}, not ${project}`);
  slot.outcome ??= followUps(job, project, slot.setup, deps);
  return { setup: slot.setup, outcome: slot.outcome };
}

async function followUps(job: string, project: string, setup: StartedSetup, deps: CarryDeps): Promise<string> {
  const { name, pool, poolLabel, access, research } = setup;
  let message = pool
    ? `${name} created.${research ? ` ${research} research ${research === 1 ? "agent is" : "agents are"} starting.` : ""}`
    : `${name} created. Research starts when you choose a pool.`;
  if (pool) {
    const assigned = await deps.assign(project, pool, deps.generation());
    if (assigned.kind !== "ok" || assigned.value.kind !== "applied") {
      message = `${name} created. Couldn't assign ${poolLabel}. Choose it again in the project's Settings, Pool.`;
    }
  }
  if (access !== "sandboxed") {
    const read = await deps.read(project);
    const saved = read.kind === "ok"
      ? await deps.save(project, policyOf(read.value, access, read.value.network, read.value.toolchains.map((row) => row.allowed), read.value.ide_git_hooks, read.value.ide_agent_files), read.value.version, deps.generation())
      : read;
    const state = saved.kind === "ok" ? receiptState(saved.value) : null;
    if (state?.kind !== "applied") {
      const why = saved.kind === "error" ? ` ${failureCopy(saved)}` : state?.kind === "rejected" ? ` ${failureCopy(state)}` : "";
      message = `${message} Its agents stay Sandboxed: ${accessLabel(access)} wasn't saved.${why} Choose it again in the project's Settings, Agent access.`;
    }
  }
  noteSetupCarried(job, message);
  return message;
}
