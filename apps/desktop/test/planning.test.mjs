import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const planning = await import("../src/screens/project/planning.tsx");
const research = await import("../src/screens/project/research.ts");
const wizard = await import("../src/screens/new-project/wizard.ts");
const fields = await import("../src/screens/new-project/fields.tsx");
const intake = await import("../src/screens/intake/intake.ts");
const api = await import("../src/api-projects.ts");
const { OnboardingReviewView } = await import("../src/screens/intake/onboarding.tsx");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const html = (type, props) => renderToStaticMarkup(createElement(type, props));
const noop = () => {};

const job = (id, kind, status, extra = {}) => ({
  id, project_id: "p1", kind, kind_label: kind === "roadmap_planning" ? "Roadmap and release process" : "Market research", status, status_label: "",
  report_path: kind === "roadmap_planning" ? "context/roadmap.md" : "context/research/market_research.md", harness: "codex", tools: true,
  attempts: 1, claimer: null, error: null, cancel_requested: false, output_bytes: 0, created_ms: 1_000, started_ms: 2_000, heartbeat_ms: null,
  finished_ms: null, ...extra,
});
const team = { master: "claude", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 };
const stored = (extra = {}) => ({
  project_id: "p1", name: "shop", about: "", tech_stack: [], platforms: [], research: [], pool: "main", team, documents: [],
  folder: "/Users/mira/Projects/shop", created_ms: 1, revision: 4, ...extra,
});
const view = (extra = {}) => ({
  project_id: "p1", graph: { kind: "done" }, mapping: { kind: "done" },
  draft: { about: "A shop for plants.", tech_stack: ["Rust"], platforms: ["website"], research: [], pool: null, team: null, documents: [] },
  facts: { languages: ["Rust"], build_commands: [], test_commands: [], entry_points: [] },
  missing: [], name: "Plant shop", overview: null, finished: false, ...extra,
});
const review = (onboarding, draft) => html(OnboardingReviewView, {
  name: "Plant shop", view: { kind: "ok", value: onboarding }, draft, catalog: undefined, errors: {}, showErrors: false, busy: false, problem: "",
  onDraft: noop, onConfirm: noop, onOpenLink: noop,
});
const checked = (markup) => /id="(np|ob)-roadmap"[^>]*aria-checked="true"/.test(markup) || /aria-checked="true"[^>]*id="(np|ob)-roadmap"/.test(markup);

test("the roadmap toggle is on by default when a project is created, imported or cloned", () => {
  const draft = wizard.emptyDraft();
  assert.equal(draft.roadmap, true);
  assert.deepEqual(wizard.buildRequest({ ...draft, name: "Shop" }).setup.research, ["market_research", "competitor_research", "design_research", "roadmap_planning"]);
  const created = html(fields.RoadmapSwitch, { prefix: "np", value: draft.roadmap, onChange: noop });
  assert.match(created, new RegExp(fields.ROADMAP_LABEL));
  assert.ok(checked(created), created);
  assert.match(read("src/screens/new-project/index.tsx"), /<RoadmapSwitch prefix="np" value=\{draft\.roadmap\}/);

  const imported = intake.onboardingDraft(view(), stored());
  assert.equal(imported.roadmap, true, "a found or cloned folder starts with planning on, whatever the mapping draft says");
  const shown = review(view(), imported);
  assert.match(shown, /Plan a roadmap after research/);
  assert.ok(checked(shown), shown);
  assert.deepEqual(wizard.editRequest(stored(), imported, imported.roadmap).setup.research, ["roadmap_planning"], "Confirm sends the toggle the owner sees");
  assert.deepEqual(wizard.editRequest(stored(), { ...imported, roadmap: false }, false).setup.research, [], "turned off, no planning job is asked for");
  const onboarding = read("src/screens/intake/onboarding.tsx");
  assert.match(onboarding, /setDraft\(onboardingDraft\(value, saved\)\)/);
  assert.match(onboarding, /const request = editRequest\(saved, draft, value\?\.finished \? undefined : draft\.roadmap\);/);
  assert.match(read("src/screens/intake/found.tsx"), /navigate\(\{ name: "onboarding", id: started\.id \}\)/, "an imported or cloned folder is set up through this review");

  const saved = intake.onboardingDraft(view({ finished: true }), stored());
  assert.equal(saved.roadmap, false, "a saved setup shows its saved choice");
  assert.equal(intake.onboardingDraft(view({ finished: true }), stored({ research: ["roadmap_planning"] })).roadmap, true);
  assert.doesNotMatch(review(view({ finished: true }), saved), /Plan a roadmap after research/, "the toggle belongs to the first setup only");
  const planned = wizard.draftOf(stored({ research: ["market_research", "roadmap_planning"] }));
  assert.deepEqual([planned.research, planned.roadmap], [["market_research"], true], "the planning kind is the toggle, never a research switch");
  assert.equal(wizard.draftOf(stored()).roadmap, false, "a project from before planning keeps it off");
  assert.equal(wizard.editRequest(stored({ research: ["market_research", "roadmap_planning"] }), planned).changed, false, "an untouched edit keeps the saved toggle");
  const before = wizard.draftOf(stored({ research: ["market_research"] }));
  const after = stored({ research: ["market_research", "roadmap_planning"], revision: 5 });
  assert.deepEqual(wizard.editRequest(after, before), { name: null, setup: wizard.editRequest(after, planned).setup, changed: false },
    "a form opened before Plan roadmap now has nothing to save");
  assert.deepEqual(wizard.editRequest(after, { ...before, about: "Plants for offices." }).setup.research, ["market_research", "roadmap_planning"],
    "saving it later keeps the planning job");
});

test("the planning state reads waiting, planning, done and failed from the real job list", () => {
  const view = (jobs) => planning.planningView(planning.planningState(jobs));
  assert.deepEqual(view([]), { word: "Not planned", tone: "neutral", line: "No roadmap yet. Plan one from the brief, research and code.", action: "plan", report: null });
  assert.equal(view([job(1, "market_research", "running")]).word, "Not planned", "with the toggle off no planning job waits for research");
  assert.deepEqual(view([job(1, "market_research", "running"), job(2, "roadmap_planning", "queued")]),
    { word: "Waiting for research", tone: "neutral", line: "The roadmap is planned once research finishes.", action: null, report: null });
  assert.equal(view([job(1, "market_research", "needs_pool"), job(2, "roadmap_planning", "needs_pool")]).word, "Waiting for research");
  assert.deepEqual(view([job(1, "market_research", "done"), job(2, "roadmap_planning", "queued")]),
    { word: "Planning", tone: "info", line: "Waiting to start", action: null, report: null });
  assert.equal(view([job(2, "roadmap_planning", "needs_pool", { error: "No account in the pool can run Codex." })]).line, "No account in the pool can run Codex.");
  assert.equal(view([job(1, "market_research", "running"), job(2, "roadmap_planning", "running")]).line, "Writing the roadmap and release process");
  assert.deepEqual(view([job(2, "roadmap_planning", "done")]),
    { word: "Done", tone: "ok", line: "The roadmap and release process are in Context.", action: "plan", report: "context/roadmap.md" });
  assert.deepEqual(view([job(2, "roadmap_planning", "done"), job(5, "roadmap_planning", "failed", { error: "The harness stopped." })]),
    { word: "Failed", tone: "danger", line: "The harness stopped.", action: "retry", report: null }, "the newest planning job decides");
  assert.deepEqual(view([job(5, "roadmap_planning", "cancelled")]),
    { word: "Stopped", tone: "neutral", line: "Planning stopped before it finished.", action: "retry", report: null });

  const jobs = [job(1, "market_research", "done"), job(2, "roadmap_planning", "running")];
  assert.deepEqual(research.researchJobs(jobs).map((item) => item.id), [1], "planning never shows as a research row");
  assert.equal(research.planningJob(jobs).id, 2);

  const line = html(planning.PlanningLineView, { view: view([job(5, "roadmap_planning", "failed")]), busy: false, problem: "", onAction: noop, onOpen: noop });
  assert.match(line, /Roadmap and release process/);
  assert.match(line, />Failed</);
  assert.match(line, />Retry</);
  const idle = html(planning.PlanningLineView, { view: view([]), busy: false, problem: "", onAction: noop, onOpen: noop });
  assert.match(idle, new RegExp(`>${planning.PLAN_NOW}<`));
  const running = html(planning.PlanningLineView, { view: view([job(2, "roadmap_planning", "running")]), busy: false, problem: "", onAction: noop, onOpen: noop });
  assert.doesNotMatch(running, /<button/, "while a job runs the line shows its state instead of an action");
});

test("Plan roadmap now and Retry ask before they can replace the roadmap", () => {
  const items = (...kinds) => ({ kind: "ok", value: { project_id: "p1", folder: "/Users/mira/Projects/shop", items: kinds.map((kind) => ({ path: kind === "plan" ? "context/roadmap.md" : "context/brief.md", kind, bytes: 10 })) } });
  assert.equal(planning.planReplaces({ kind: "idle" }, items("brief")), false, "nothing to replace, so it plans at once");
  assert.equal(planning.planReplaces({ kind: "idle" }, items("brief", "plan")), true, "a roadmap the owner wrote by hand");
  const linkedRoadmap = { kind: "ok", value: { project_id: "p1", folder: "/Users/mira/Projects/shop", items: [{ path: "repo/docs/roadmap.md", kind: "plan", bytes: 10, linked: true }] } };
  assert.equal(planning.planReplaces({ kind: "idle" }, linkedRoadmap), false, "a roadmap linked from the repo is never written by planning");
  assert.equal(planning.planReplaces({ kind: "idle" }, undefined), true, "the context isn't read yet");
  assert.equal(planning.planReplaces({ kind: "done", job: job(2, "roadmap_planning", "done") }, items("brief")), true, "a plan ran before");
  assert.equal(planning.planReplaces({ kind: "failed", job: job(2, "roadmap_planning", "failed") }, items("brief")), true, "Retry asks too");
  const source = read("src/screens/project/planning.tsx");
  assert.match(source, /if \(planReplaces\(state, data\.context\)\) setAsking\(true\); else void act\(\);/);
  assert.match(source, /<ConfirmDialog open=\{asking\} title="Replace the roadmap\?" confirmLabel="Replace"/);
  assert.match(planning.REPLACE_LINE, /Your edits to them are lost\.$/);
});

test("Retry and Plan roadmap now call the engine", () => {
  assert.equal(planning.planningCall("p1", { kind: "waiting" }), null);
  assert.equal(planning.planningCall("p1", { kind: "planning", job: job(2, "roadmap_planning", "running") }), null);
  const source = read("src/screens/project/planning.tsx");
  assert.match(source, /if \(action === "retry" && state\.kind === "failed"\) return retryResearch\(id, state\.job\.id, nextGeneration\(\)\);/);
  assert.match(source, /if \(action === "plan"\) return planRoadmap\(id, nextGeneration\(\)\);/);
  assert.match(read("src/api-projects.ts"), /export function planRoadmap\(id: string, generation: number\): Promise<Result<IntentReceipt>> \{\s*return call<IntentReceipt>\("desktop_plan_roadmap", \{ id, generation \}\);/);
  assert.match(read("src/engine-client.ts"), /export function planRoadmap\(id: string, generation: number\): Promise<IntentReceipt> \{\s*return invoke<IntentReceipt>\("desktop_plan_roadmap", \{ id, generation \}\);/);
  assert.match(read("src/screens/project/overview.tsx"), /<PlanningLine data=\{data\} \/>/);
  assert.match(read("src/screens/project/settings.tsx"), /<GeneralSection data=\{data\} setup=\{setup!\} \/><PlanningLine data=\{data\} \/>/);
  assert.match(read("src-tauri/src/lib.rs"), /lock\(&state\)\.await\?\.plan_roadmap\(&id, generation\)\.await/);
});

test("planning_busy shows its sentence", () => {
  const sentence = "A roadmap is already being planned for this project. Wait for it to finish.";
  assert.equal(api.failureSentence("planning_busy"), sentence);
  assert.equal(api.failureSentence("setup_unconfirmed"), "Confirm this project's setup first. Its setup review has the roadmap switch.");
  const receipt = { kind: "ok", value: {
    kind: "plan_roadmap", generation: "4", project_id: "p1", baseline_id: null, model_access: null, project_spec: null, operation_id: "o",
    outcome: "rejected", pending: false, rejection: "planning_busy",
  } };
  assert.deepEqual(research.changeOutcome(receipt, "Roadmap planning is queued.", "planning the roadmap"), { toast: "", note: sentence, reload: true });
  const applied = { kind: "ok", value: { ...receipt.value, outcome: "applied", rejection: null } };
  assert.deepEqual(research.changeOutcome(applied, "Roadmap planning is queued.", "planning the roadmap"), { toast: "Roadmap planning is queued.", note: "", reload: true });
  assert.match(html(planning.PlanningLineView, { view: planning.planningView({ kind: "idle" }), busy: false, problem: sentence, onAction: noop, onOpen: noop }),
    /role="alert">A roadmap is already being planned for this project\. Wait for it to finish\.</);
});

test("the Context tab lists the roadmap and release process beside the brief", () => {
  const source = read("src/screens/project/context.tsx");
  assert.match(source, /items\.filter\(\(entry\) => entry\.kind === "plan"\)/);
  assert.match(source, /researchKindOf\(chosen, researchJobs\(jobs\)\)/, "a planning document never offers Save research to memory");
});
