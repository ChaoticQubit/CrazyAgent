import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");
const planning = await import("../src/screens/project/planning.tsx");
const overview = await import("../src/screens/project/overview.tsx");
const api = await import("../src/api-projects.ts");

const job = (id, kind, status, retryable) => ({
  id, project_id: "p-1", kind, kind_label: kind === "roadmap_planning" ? "Roadmap and release process" : "Market research", status, status_label: "",
  report_path: kind === "roadmap_planning" ? "context/roadmap.md" : "context/research/market_research.md", harness: "codex", tools: true,
  attempts: 1, claimer: null, error: null, cancel_requested: false, output_bytes: 0, created_ms: 1_000, started_ms: 1_000, heartbeat_ms: null,
  finished_ms: 2_000, retryable,
});

const tabData = (jobs) => ({
  id: "p-1", name: "Harbor Notes", setup: undefined, research: { kind: "ok", value: { project_id: "p-1", jobs } }, context: undefined,
  pools: undefined, harnesses: undefined, assigned: null, now: 1_000 + 60_000, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
  team: { result: undefined, at: 0 }, reloadTeam: () => {},
});

const planned = (jobs) => planning.planningView(planning.planningState(jobs));

test("a research agent offers Try again only when the engine says it can run again", () => {
  for (const status of ["failed", "cancelled"]) {
    assert.equal(research.researchActions(job(1, "market_research", status, true)).retry, true, status);
    assert.equal(research.researchActions(job(1, "market_research", status, false)).retry, false, status);
  }
});

test("the Activity research row offers Try again only when the engine says it can run again", () => {
  const shown = (retryable) => renderToStaticMarkup(createElement(overview.ActivityPanel, { data: tabData([job(1, "market_research", "failed", retryable)]) }));
  assert.equal(shown(true).match(/>Try again</g)?.length, 1);
  assert.doesNotMatch(shown(false), />Try again</);
});

test("the roadmap line offers Retry only when the engine says the planning job can run again", () => {
  for (const status of ["failed", "cancelled"]) {
    assert.equal(planned([job(2, "roadmap_planning", status, true)]).action, "retry", status);
    assert.equal(planned([job(2, "roadmap_planning", status, false)]).action, null, status);
    assert.equal(planning.planningCall("p-1", { kind: "failed", job: job(2, "roadmap_planning", status, false) }), null, status);
  }
});

test("a retry the engine refuses where research does not run reads as a plain line", () => {
  assert.equal(api.failureSentence("research_unavailable_here"),
    "Research does not run on this computer in this version, so it cannot run again.");
});
