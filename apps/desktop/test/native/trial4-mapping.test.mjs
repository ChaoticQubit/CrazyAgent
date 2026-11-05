import assert from "node:assert/strict";
import test from "node:test";
import { CANARY, DECOY, RESEARCH_DOCS } from "../../../../scripts/trial/mapping-docs.mjs";
import * as mapping from "./trial4/n-map-mapping.mjs";

/** The mapping row the 18.14 onboarding view drew while the mapping ran: rendered by the stage
 * aae6a477 view, which still had the 18.14 setup screen (map-capture/capture-1814.json). */
const ROW_1814 = { title: "Map the project", line: null, word: "Working" };

/** How the 18.14 mapping ended in trial 4: run 1 failed the step at 902 s, and the ca-trial-4c
 * onboarding row of run 3 reads failed, "Mapping agent ran out of time". */
const END_1814 = { ms: 902000, title: "Map the project", line: "Mapping agent ran out of time", word: "Failed" };

/** The competitor and market research switches of the 18.14 setup review, rendered as ROW_1814 was. */
const ITEMS_1814 = [
  { label: "Market research", notes: ["Who needs this, how big the market is, and how people pay today."] },
  { label: "Competitor research", notes: ["Products that already do this, their features, prices and gaps."] },
];

/** The facts of the ca-trial-4b onboarding row the 18.14 engine drafted, read from the owner's
 * engine database read-only: no research and no documents. */
const FACTS_1814 = { languages: ["Rust", "Python"], build_commands: ["cargo build"], test_commands: ["cargo test"], entry_points: ["src/lib.rs"] };

const ID = "ca-trial-4d";

test("MAP-1 and MAP-2 run after open-app, before the add-project step finishes the setup they leave at its review", () => {
  assert.deepEqual(mapping.checks.map((check) => [check.id, check.op, check.after]), [["MAP-1", ["MAP-1"], "open-app"], ["MAP-2", ["MAP-2"], "open-app"]]);
  for (const check of mapping.checks) assert.match(check.why, /^owner: /);
});

test("a progress line names the try, the tries and the whole minutes; any other line is none", () => {
  assert.deepEqual(mapping.tryOf("Mapping, try 1 of 3, under 1 min."), { number: 1, tries: 3, minutes: 0 });
  assert.deepEqual(mapping.tryOf("Mapping, try 2 of 3, 4 min. Try 1 failed: Mapping agent ran out of time"), { number: 2, tries: 3, minutes: 4 });
  assert.equal(mapping.tryOf(null), null);
  assert.equal(mapping.tryOf("Mapping agent ran out of time"), null);
  assert.equal(mapping.tryOf("Mapping, try 4 of 3, 1 min."), null);
});

test("MAP-1 fails the 18.14 mapping: only Working for over 60 s, and no Done within 15 min", () => {
  assert.match(mapping.progressFault([{ ms: 0, ...ROW_1814 }, { ms: 61000, ...ROW_1814 }]), /showed only "Working" for 61 s with no try line/);
  assert.equal(mapping.progressFault([{ ms: 0, ...ROW_1814 }, { ms: 30000, ...ROW_1814, word: "Done" }], true), "the mapping never showed its progress line");
  assert.match(mapping.endFault(END_1814), /the mapping ended Failed after 902 s: Mapping agent ran out of time/);
  assert.match(mapping.endFault({ ...END_1814, word: "Working", line: null }), /the mapping still read Working after 902 s/);
});

test("MAP-1 passes a mapping that names its try within 60 s, retries and ends Done within 15 min", () => {
  const waiting = { ms: 0, title: "Map the project", line: null, word: "Waiting" };
  const samples = [waiting, { ...waiting, ms: 70000, word: "Working" }, { ...waiting, ms: 129000, word: "Working", line: "Mapping, try 1 of 3, under 1 min." },
    { ...waiting, ms: 400000, word: "Working", line: "Mapping, try 2 of 3, 1 min. Try 1 failed: Mapping agent ran out of time" }];
  assert.equal(mapping.progressFault(samples), null);
  assert.match(mapping.progressFault([...samples.slice(0, 2), { ...waiting, ms: 131000, word: "Working" }]), /showed only "Working" for 61 s/);
  assert.equal(mapping.endFault({ ...waiting, ms: 600000, word: "Done" }), null);
  assert.match(mapping.endFault({ ...waiting, ms: 900001, word: "Done" }), /after 900 s, over the 900 s/);
});

test("MAP-1 waits through a paused pool and reports BLOCKED, not FAIL, when the pool is still paused at the end", () => {
  const working = { title: "Map the project", line: null, word: "Working" };
  const paused = { ...working, word: "Rate limited until 01:40" };
  for (const word of ["Working", "Waiting", paused.word]) assert.equal(mapping.stillRunning(word), true, word);
  for (const word of ["Done", "Failed", "Off", "Not installed"]) assert.equal(mapping.stillRunning(word), false, word);
  assert.match(mapping.endFault({ ...paused, ms: 901000 }), /^BLOCKED: the project's pool was still paused after 901 s \(Rate limited until 01:40\)/);
  assert.equal(mapping.progressFault([{ ...working, ms: 0 }, { ...paused, ms: 30000 }, { ...paused, ms: 600000 }, { ...working, ms: 601000 }, { ...working, ms: 650000 }]), null, "a pause restarts the 60 s");
  assert.match(mapping.progressFault([{ ...working, ms: 0 }, { ...paused, ms: 30000 }, { ...working, ms: 601000 }, { ...working, ms: 662000 }]), /showed only "Working" for 61 s/);
});

test("MAP-2 fails the 18.14 review and row: no research kind names a seeded document", () => {
  assert.deepEqual(mapping.repoFiles(ITEMS_1814), { "Market research": [], "Competitor research": [] });
  assert.deepEqual(mapping.missingInReview(mapping.repoFiles(ITEMS_1814)), [`Competitor research: ${RESEARCH_DOCS.competitor}`, `Market research: ${RESEARCH_DOCS.market}`]);
  const row = mapping.rowResearch({ project_id: ID, mapping: { kind: "done" }, facts: FACTS_1814 }, ID);
  assert.deepEqual(row, {});
  assert.deepEqual(mapping.missingInRow(row), [`competitor_research: ${RESEARCH_DOCS.competitor}`, `market_research: ${RESEARCH_DOCS.market}`]);
});

test("MAP-2 passes a review and a row that name the seeded competitor and market documents", () => {
  const items = [
    { label: "Market research", notes: ["Who needs this, how big the market is, and how people pay today.", `Already in the repo: ${RESEARCH_DOCS.market}`] },
    { label: "Competitor research", notes: ["Products that already do this, their features, prices and gaps.", `Already in the repo: ${RESEARCH_DOCS.competitor}, docs/other.md`] },
    { label: "Front-end design", notes: ["Screens and layout.", `Already in the repo: ${RESEARCH_DOCS.design}`] },
  ];
  assert.deepEqual(mapping.repoFiles(items)["Competitor research"], [RESEARCH_DOCS.competitor, "docs/other.md"]);
  assert.deepEqual(mapping.missingInReview(mapping.repoFiles(items)), []);
  const facts = { ...FACTS_1814, research: [{ kind: "competitor_research", files: [RESEARCH_DOCS.competitor] }, { kind: "market_research", files: [RESEARCH_DOCS.market] }], documents: [RESEARCH_DOCS.design] };
  const row = mapping.rowResearch({ project_id: ID, facts }, ID);
  assert.deepEqual(row, { competitor_research: [RESEARCH_DOCS.competitor], market_research: [RESEARCH_DOCS.market] });
  assert.deepEqual(mapping.missingInRow(row), []);
});

test("MAP-2 reads the onboarding row only from this project's answer, never from no read", () => {
  assert.throws(() => mapping.rowResearch({ failed: "engine_unavailable" }, ID), /desktop_onboarding failed: engine_unavailable/);
  assert.throws(() => mapping.rowResearch({ project_id: "ca-trial-4c", facts: FACTS_1814 }, ID), /answered for "ca-trial-4c", not ca-trial-4d/);
  assert.throws(() => mapping.rowResearch({ project_id: ID }, ID), /desktop_onboarding answered no mapping facts for ca-trial-4d/);
});

test("MAP-2 claims only that the decoy is never listed or shown, and names the engine unit test that proves it is never read", () => {
  const check = mapping.checks.find((one) => one.id === "MAP-2");
  assert.match(check.title, /never lists or shows the secret-named decoy$/);
  assert.match(mapping.NEVER_READ, /secret_named_documents_are_neither_read_nor_listed/);
  assert.match(mapping.NEVER_READ, /7a8f4d2e/);
});

test("the decoy's name or canary anywhere is a leak; the research documents are not", () => {
  assert.deepEqual(mapping.leaks(`Project documents found\n${RESEARCH_DOCS.design}\n${RESEARCH_DOCS.competitor}`), []);
  assert.deepEqual(mapping.leaks(`Project documents found\n${DECOY}`), [DECOY]);
  assert.deepEqual(mapping.leaks(JSON.stringify({ facts: { documents: ["notes/API-KEYS.md"] } })), [DECOY]);
  assert.deepEqual(mapping.leaks(`"overview": "keys: ${CANARY}"`), [CANARY]);
});
