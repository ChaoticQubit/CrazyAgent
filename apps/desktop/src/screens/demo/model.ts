import { accountLabels, type AccountView, type PoolSummary, type Result } from "../../api";
import type { DemoView } from "../../api-demo";
import { failureCopy, needsReconcile, receiptState, settledReceipt, type ReceiptState } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import type { Loaded } from "../../state";
import { formatRoute } from "../../ui/router";
import { DEMO_ENTRY, DEMO_NOT_READY, DEMO_POOL_REASONS, DEMO_POOL_UNASSIGNED, DEMO_POOL_UNKNOWN, DEMO_REFUSALS, DEMO_STEPS, DEMO_UNCONFIRMED, type DemoStepId } from "./words";

const READY_ACCOUNT = new Set(["signed_in", "key_saved"]);
const DEMO_POOLS_MAX = 64;

/** How often the demo's page and guide read the demo again while a step is still open. */
export const DEMO_POLL_MS = 5000;

/** One pool on the demo page: its id, its name, the names of its accounts that are ready for
 * agents, and why the demo cannot run on it, or null when it can. */
export interface DemoPoolChoice {
  id: string;
  name: string;
  ready: string[];
  reason: string | null;
}

/** The Home entry's one action: Try the demo before the demo exists, Open the demo once it
 * does, with the words and the link. */
export interface DemoEntry {
  action: "start" | "open";
  label: string;
  href: string;
}

/** One guide step with its words, whether the engine saw it happen and where it leads. */
export interface GuideStep {
  id: DemoStepId;
  title: string;
  body: string;
  done: boolean;
  href: string;
}

/** The guide on the demo project's page: every step in order and the first one not done. */
export interface Guide {
  steps: GuideStep[];
  current: number;
}

function poolReason(code: string): string {
  return Object.hasOwn(DEMO_POOL_REASONS, code) ? DEMO_POOL_REASONS[code] : `${DEMO_POOL_UNKNOWN} (${code})`;
}

/** Lists the pools the demo page offers, in the engine's order, each with the reason the
 * engine gives when the demo cannot run on it (an rcli-only pool, no ready account). Names and
 * members come from the pool summaries and account names from Accounts, joined by id; an
 * account not ready for agents is left out of the ready names. Inputs: the demo read, the pool
 * summaries and the accounts. Output: the choices, at most 64. */
export function demoPoolChoices(view: DemoView, pools: readonly PoolSummary[], accounts: readonly AccountView[]): DemoPoolChoice[] {
  const labels = accountLabels(accounts);
  const status = new Map(accounts.map((account) => [account.id, account.status]));
  const summaries = new Map(pools.map((pool) => [pool.id, pool]));
  return view.pools.slice(0, DEMO_POOLS_MAX).map(({ pool, unavailable }) => {
    const summary = summaries.get(pool);
    const ready = (summary?.members ?? []).filter((member) => READY_ACCOUNT.has(status.get(member.id) ?? "")).map((member) => labels.get(member.id) ?? member.id);
    const code = unavailable ?? null;
    return { id: pool, name: summary?.name.trim() || pool, ready, reason: code === null ? null : poolReason(code) };
  });
}

/** Whether Create the demo may be sent: only once the owner picked a pool the demo can run
 * on. Inputs: the choices and the picked pool id, null until one is picked. Output: true when
 * the pick is listed and has no reason against it. */
export function pickable(choices: readonly DemoPoolChoice[], picked: string | null): boolean {
  return picked !== null && choices.some((choice) => choice.id === picked && choice.reason === null);
}

/** The Home entry's action from the demo read. Input: the read. Output: Open the demo while
 * the demo project exists, Try the demo otherwise, or null until the read is good. */
export function demoEntry(view: Loaded<DemoView>): DemoEntry | null {
  if (view.kind !== "ok") return null;
  const id = view.value.project_id;
  if (id) return { action: "open", label: DEMO_ENTRY.open, href: formatRoute({ name: "project", id, tab: "conversation" }) };
  return { action: "start", label: DEMO_ENTRY.start, href: formatRoute({ name: "demo" }) };
}

/** The sentence for a refused Create the demo: the demo's own words for its codes, else the
 * project screens' sentence, which names an unknown code. Input: the failure. Output: one line. */
export function refusalCopy(failure: { code: string; cause?: string }): string {
  return Object.hasOwn(DEMO_REFUSALS, failure.code) ? DEMO_REFUSALS[failure.code] : failureCopy(failure);
}

function stepDone(id: DemoStepId, view: DemoView): boolean {
  switch (id) {
    case "goal": return view.steps.goal_sent;
    case "agents": return view.steps.agent_started;
    case "question": return view.steps.question_answered;
    case "changes": return view.steps.change_set_accepted;
    case "delete": return false;
  }
}

function stepHref(id: DemoStepId, project: string): string {
  switch (id) {
    case "goal": return formatRoute({ name: "project", id: project, tab: "conversation" });
    case "agents": return formatRoute({ name: "project", id: project, tab: "agents" });
    case "question": return formatRoute({ name: "project", id: project, tab: "questions" });
    case "changes": return formatRoute({ name: "project", id: project, tab: "changes" });
    case "delete": return `#/projects/${encodeURIComponent(project)}/settings?section=remove&ask=remove`;
  }
}

/** The guide for one project page: each step ticked from the engine's demo read and linked to
 * the tab where it happens; the last opens Remove, where Delete everything moves the demo to
 * the Trash. Inputs: the demo read and the page's project id. Output: the guide, or null on
 * any page but the demo project's. */
export function guideSteps(view: DemoView, projectId: string): Guide | null {
  if (!view.project_id || view.project_id !== projectId) return null;
  const steps = DEMO_STEPS.map((step) => ({ id: step.id, title: step.title, body: step.body, done: stepDone(step.id, view), href: stepHref(step.id, projectId) }));
  return { steps, current: steps.findIndex((step) => !step.done) };
}

/** Whether a project is the demo, for its Demo mark. Inputs: the demo read and the project id.
 * Output: true only when a good read names that project. */
export function isDemoProject(view: Loaded<DemoView>, projectId: string): boolean {
  return view.kind === "ok" && view.value.project_id === projectId;
}

/** The calls Create the demo makes, passed in so the order can be held by a test: send the
 * create, reconcile a lost reply once, read the demo when the answer stays unconfirmed, give
 * the pool to the new project, and the next intent generation for each change. */
export interface DemoMaker {
  create: (pool: string, generation: number) => Promise<Result<IntentReceipt>>;
  reconcile: () => Promise<Result<IntentReceipt>>;
  read: () => Promise<Result<DemoView>>;
  assign: (project: string, pool: string, generation: number) => Promise<Result<ReceiptState>>;
  generation: () => number;
}

/** What Create the demo ended with: the demo made, with a line when its pool could not be
 * given to it, or refused with the sentence that says why. */
export type DemoMade = { kind: "made"; project: string; note: string | null } | { kind: "refused"; message: string };

/** Makes the demo on the picked pool the way New Project makes a project: sends Create the
 * demo, reconciles a lost reply once, and gives the same pool to the demo once it exists,
 * since the engine leaves pool settings to that one path. An answer still unconfirmed is
 * settled by reading the demo, as the TUI does; a refused create gives no pool. Inputs: the
 * pool id and the calls. Output: the outcome. */
export async function makeDemo(pool: string, maker: DemoMaker): Promise<DemoMade> {
  const sent = await maker.create(pool, maker.generation());
  if (sent.kind === "not_ready") return { kind: "refused", message: DEMO_NOT_READY };
  if (sent.kind !== "ok") return { kind: "refused", message: refusalCopy(sent) };
  const receipt = needsReconcile(sent.value) ? settledReceipt(sent.value, await maker.reconcile()) : sent.value;
  const state = receiptState(receipt);
  if (state.kind === "rejected") return { kind: "refused", message: refusalCopy({ code: state.code }) };
  const project = state.kind === "applied" && receipt.project_id ? receipt.project_id : await madeDemo(maker);
  if (project === null) return { kind: "refused", message: DEMO_UNCONFIRMED };
  const given = await maker.assign(project, pool, maker.generation());
  const note = given.kind === "ok" && given.value.kind === "applied" ? null : DEMO_POOL_UNASSIGNED;
  return { kind: "made", project, note };
}

/** The demo's id when a fresh read finds the demo, which settles a Create the demo whose
 * answer stayed unconfirmed. Input: the calls. Output: the id, or null when the read fails
 * or finds no demo. */
async function madeDemo(maker: DemoMaker): Promise<string | null> {
  const read = await maker.read();
  return read.kind === "ok" ? read.value.project_id : null;
}
