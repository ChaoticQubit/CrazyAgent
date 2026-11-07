import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-questions.ts");
const view = await import("../src/screens/project/questions.tsx");

const NOW = Date.UTC(2026, 9, 27, 15, 0);
const question = (extra = {}) => ({
  id: 12, project_id: "p-1", asker: "agent:team-3", question: "Which database should the app use?", options: ["Postgres", "SQLite"],
  recommended: "SQLite", context: "It runs on one laptop.", status: "pending", version: 3, answer: null, created_ms: NOW - 120_000, settled_ms: null,
  ...extra,
});
const card = (item, extra = {}) => renderToStaticMarkup(createElement(view.QuestionCard, {
  question: item, now: NOW, busy: false, onAnswer: () => {}, onDismiss: () => {}, ...extra,
}));

test("pending questions come first, the longest waiting on top, then the most recently settled", () => {
  const ordered = client.orderQuestions([
    question({ id: 1, status: "answered", answer: "Yes", settled_ms: NOW - 50_000 }),
    question({ id: 2, created_ms: NOW - 10_000 }),
    question({ id: 3, status: "dismissed", settled_ms: NOW - 1_000 }),
    question({ id: 4, created_ms: NOW - 90_000 }),
    question({ id: 5, status: "answered_in_terminal", settled_ms: NOW - 70_000 }),
  ]);
  assert.deepEqual(ordered.map((item) => item.id), [4, 2, 3, 1, 5]);
  const merged = view.mergedQuestions({ items: [question({ id: 7, version: 4 })], next: 3 }, [question({ id: 7, version: 2 }), question({ id: 6, status: "answered", settled_ms: NOW - 1 })]);
  assert.deepEqual(merged.map((item) => [item.id, item.version]), [[7, 4], [6, 3]]);
});

test("a pending card names who asked, marks the recommended option and waits for a choice", () => {
  const markup = card(question());
  assert.match(markup, /aria-label="Question from Agent team-3"/);
  assert.match(markup, />Waiting for you</);
  assert.match(markup, />2 min ago</);
  assert.match(markup, />Which database should the app use\?</);
  assert.match(markup, /<summary>Context<\/summary><p>It runs on one laptop.<\/p>/);
  assert.equal(markup.match(/>Recommended</g)?.length, 1);
  assert.match(markup, /<span>SQLite<\/span><span class="qs-recommended pj-small">Recommended<\/span>/);
  assert.doesNotMatch(markup, /<span>Postgres<\/span><span class="qs-recommended/);
  assert.match(markup, /<label for="[^"]+">Or write your own answer<\/label>/);
  assert.match(markup, /<button[^>]*disabled=""[^>]*>Answer<\/button>/, "Answer waits for an option or text");
  assert.match(markup, /<button[^>]*>Dismiss<\/button>/);
  assert.doesNotMatch(markup, /<button[^>]*disabled=""[^>]*>Dismiss<\/button>/);
  assert.equal(client.isRecommended(question(), 1), true);
  assert.equal(client.isRecommended(question(), 0), false);
  assert.equal(client.isRecommended(question({ recommended: null }), 1), false);
  const busy = card(question(), { busy: true });
  assert.match(busy, /<button[^>]*disabled=""[^>]*>Dismiss<\/button>/);
});

test("a question with no options offers its suggested answer and a field of its own", () => {
  const markup = card(question({ options: [], recommended: "Keep it private" }));
  assert.match(markup, />Suggested: Keep it private</);
  assert.match(markup, />Use suggested answer</);
  assert.match(markup, /<label for="[^"]+">Your answer<\/label>/);
  assert.doesNotMatch(markup, /type="radio"/);
  assert.doesNotMatch(card(question({ options: [], recommended: null })), /Suggested/);
});

test("a question asked in an agent's terminal is answered there, so its card offers only Dismiss", () => {
  const asked = question({ asker: "terminal:2", options: ["Yes", "No"], recommended: "Yes" });
  assert.equal(client.askedInTerminal(asked), true);
  assert.equal(client.askedInTerminal(question()), false);
  const markup = card(asked);
  assert.match(markup, />Asked in the terminal of agent 2</);
  assert.match(markup, /marked answered here when the question leaves the screen\.</);
  assert.doesNotMatch(markup, /type="radio"|<textarea|>Answer<\/button>|Use suggested answer/);
  assert.match(markup, /<button[^>]*>Dismiss<\/button>/);
  assert.doesNotMatch(card(question({ asker: "terminal:2", status: "answered_in_terminal", settled_ms: NOW })), />Dismiss</);
});

test("the answer sent is one option or trimmed text of at most 4096 bytes", () => {
  assert.deepEqual(view.choiceOf(1, ""), { option: 1 });
  assert.deepEqual(view.choiceOf(0, "ignored while an option is picked"), { option: 0 });
  assert.deepEqual(view.choiceOf(null, "  Use SQLite  "), { text: "Use SQLite" });
  assert.equal(view.choiceOf(null, ""), null);
  assert.equal(view.choiceOf(null, " \n\t "), null);
  assert.equal(client.typedAnswer("a".repeat(client.ANSWER_BYTES_MAX)), "a".repeat(client.ANSWER_BYTES_MAX));
  assert.equal(client.typedAnswer("a".repeat(client.ANSWER_BYTES_MAX + 1)), null);
  assert.equal(client.typedAnswer("é".repeat(client.ANSWER_BYTES_MAX / 2)), "é".repeat(client.ANSWER_BYTES_MAX / 2));
  assert.equal(client.typedAnswer("é".repeat(client.ANSWER_BYTES_MAX / 2 + 1)), null, "the bound is bytes, not characters");
  assert.equal(client.ANSWER_BYTES_MAX, 4096);
});

test("agent text is shown through displayText, so hidden characters cannot hide or reorder it", async () => {
  const { displayText } = await import("../src/screens/conversation.tsx");
  const tricky = "Approve‮?stnemyap​";
  const markup = card(question({ question: tricky, context: `Note\u0007${tricky}`, options: [tricky, "No"], recommended: "No", asker: "agent:x⁦y" }));
  assert.doesNotMatch(markup, /[‮​⁦\u0007]/u);
  assert.ok(markup.includes(displayText(tricky)), displayText(tricky));
  assert.ok(markup.includes(`Agent ${displayText("x⁦y")}`));
  const settled = card(question({ status: "answered", answer: `Yes‮`, settled_ms: NOW }));
  assert.doesNotMatch(settled, /‮/u);
  assert.ok(settled.includes(displayText("Yes‮")));
});

test("a settled question shows its status and answer, with no answer controls", () => {
  const answered = card(question({ status: "answered", answer: "SQLite", settled_ms: NOW - 1000 }));
  assert.match(answered, />Answered</);
  assert.match(answered, /<span class="pj-secondary">Answer: <\/span>SQLite/);
  assert.doesNotMatch(answered, /type="radio"|<textarea|>Dismiss</);
  assert.match(card(question({ status: "dismissed", settled_ms: NOW })), />Dismissed</);
  assert.match(card(question({ status: "answered_in_terminal", answer: "2", settled_ms: NOW })), />Answered in the terminal</);
  assert.deepEqual(client.statusBadge("pending"), { word: "Waiting for you", tone: "warn" });
  assert.deepEqual(client.statusBadge("dismissed"), { word: "Dismissed", tone: "neutral" });
});

test("a question settled from an older page never stays waiting: the older pages are dropped and read again", () => {
  const empty = { items: [], next: null };
  const opened = view.olderAfter(view.olderAfter(empty, { kind: "first", next: 9 }), { kind: "older", page: { items: [question({ id: 6 })], next: 3 } });
  assert.deepEqual(opened.items.map((item) => item.id), [6]);
  assert.equal(opened.next, 3);
  assert.deepEqual(view.olderAfter(opened, { kind: "first", next: 9 }), opened, "a reread of the newest page keeps the older pages open");
  const settled = view.olderAfter(opened, { kind: "settled" });
  assert.deepEqual(settled, empty);
  const reread = { items: [question({ id: 7 })], next: 9 };
  assert.deepEqual(view.mergedQuestions(reread, settled.items).map((item) => item.id), [7], "the stale older copy of question 6 is gone");
  assert.deepEqual(view.olderAfter(settled, { kind: "first", next: 9 }), { items: [], next: 9 }, "Show older questions comes back with the next read");
});

test("settling tells the owner what the asker hears: agents once, an engine step through the master, a terminal nothing", () => {
  assert.equal(client.settledNote("answered", "agent:3"), "Answered. The agent that asked gets your answer once.");
  assert.equal(client.settledNote("dismissed", "master:1"), "Dismissed. The agent that asked is told once.");
  assert.equal(client.settledNote("answered", "engine:mapping"), "Answered. The project's master gets your answer once, if its conversation has room.");
  assert.equal(client.settledNote("dismissed", "engine:research:market_research"), "Dismissed. The project's master is told once, if its conversation has room.");
  assert.equal(client.settledNote("dismissed", "terminal:2"), "Dismissed. Nothing is sent to the agent's terminal.");
  assert.match(client.dismissNote("agent:3"), /^The agent that asked is told you dismissed it, once\./);
  assert.match(client.dismissNote("terminal:2"), /^Nothing is sent to the agent's terminal\./);
  assert.match(client.dismissNote("engine:planning"), /^The CrazyAgent step that asked takes no action on its own\./);
  for (const asker of ["terminal:2", "engine:mapping"]) assert.doesNotMatch(client.dismissNote(asker), /agent that asked is told/);
});

test("askers read as plain words", () => {
  assert.equal(client.askerLabel("master:s-1"), "Master");
  assert.equal(client.askerLabel("agent:team-3"), "Agent team-3");
  assert.equal(client.askerLabel("engine:mapping"), "Project mapping");
  assert.equal(client.askerLabel("engine:planning"), "Roadmap planning");
  assert.equal(client.askerLabel("engine:research:market_research"), "Research: market research");
  assert.equal(client.askerLabel("engine:landing:cs-1"), "Landing changes");
  assert.equal(client.askerLabel("master:session:s-9"), "Master");
  assert.equal(client.askerLabel("terminal:2"), "Asked in the terminal of agent 2");
  assert.equal(client.askerLabel("someone"), "someone");
});

test("a stale answer says the question changed, and every questions code reads as a sentence", () => {
  assert.equal(client.questionsRefusal({ code: "questions_stale" }), "This question changed. Review it again.");
  assert.equal(client.STALE_LINE, "This question changed. Review it again.");
  assert.equal(client.questionsRefusal({ code: "questions_outcome_unknown" }), "CrazyAgent couldn't confirm what happened. Check the list before you try again.");
  assert.equal(client.questionsRefusal({ code: "made_up_code" }), "Something went wrong. Try again. (made_up_code)");
  for (const code of client.QUESTIONS_REFUSALS) {
    const line = client.questionsRefusal({ code });
    assert.match(line, /^[A-Z].*\.$/, code);
    assert.doesNotMatch(line, /_/, `${code} reads as a sentence: ${line}`);
  }
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/questions.rs", import.meta.url), "utf8");
  const start = engine.indexOf("pub const QUESTIONS_CODES");
  assert.ok(start >= 0, "the engine lists its questions codes");
  const own = [...engine.slice(start, engine.indexOf("];", start)).matchAll(/"([a-z_]+)"/g)].map((match) => match[1]);
  assert.ok(own.includes("owner_busy") && own.includes("versioned_unavailable"), "connection and versioning codes are read too");
  assert.ok(own.length >= 3, `engine questions codes: ${own.length}`);
  const transport = ["questions_request_bounds", "questions_reply_bounds", "questions_request_invalid"];
  const askSide = ["questions_ask_invalid", "questions_asker_full", "questions_full", "questions_not_terminal", "questions_notes_invalid", "questions_owner_turn_invalid", "questions_page_unavailable"];
  for (const code of askSide) assert.ok(own.includes(code), `${code} is still an engine code; drop it from the ask-side list when it goes`);
  for (const code of own.filter((code) => ![...transport, ...askSide].includes(code))) {
    assert.ok(client.QUESTIONS_REFUSALS.includes(code), `${code} has its own sentence`);
  }
  assert.equal(client.questionsRefusal({ code: "questions_in_terminal" }), "This question was asked in an agent's terminal. Answer it there.");
  assert.equal(client.notReady({ kind: "not_ready" }), true);
  assert.equal(client.notReady({ kind: "error", code: "questions_unavailable", message: "" }), true);
  assert.equal(client.notReady({ kind: "error", code: "questions_stale", message: "" }), false);
});

test("a project's flag counts its pending questions once, with its other unseen notifications", () => {
  const unseen = new Map([["p-1", 3], ["p-2", 1]]);
  const questionAlerts = client.unseenQuestionAlerts([
    { project_id: "p-1", unseen: 3, questions: 2 },
    { project_id: "p-2", unseen: 1, questions: 0 },
    { project_id: "p-5", unseen: 1 },
  ]);
  assert.deepEqual([...questionAlerts], [["p-1", 2]]);
  const beyondTheList = client.unseenQuestionAlerts([{ project_id: "p-6", unseen: 30, questions: 30 }]);
  assert.deepEqual([...client.withPendingQuestions(new Map([["p-6", 30]]), beyondTheList, new Map([["p-6", 30]]))], [["p-6", 30]], "thirty pending questions count thirty, not more, however few notifications the list holds");
  const pending = client.pendingByProject([{ project_id: "p-1", pending: 2 }, { project_id: "p-3", pending: 4 }, { project_id: "p-4", pending: 0 }]);
  assert.deepEqual([...pending], [["p-1", 2], ["p-3", 4]]);
  const flags = client.withPendingQuestions(unseen, questionAlerts, pending);
  assert.deepEqual(Object.fromEntries(flags), { "p-1": 3, "p-2": 1, "p-3": 4 });
  assert.deepEqual([...client.withPendingQuestions(new Map([["p-1", 1]]), new Map([["p-1", 5]]), new Map())], [], "a seen-out count never goes below zero");
  assert.deepEqual([...client.withPendingQuestions(new Map(), new Map(), new Map([["p-9", 1]]))], [["p-9", 1]], "an answered-later question stays counted while pending");
});

test("the Questions tab sits after Changes and the commands go through the bridge with only ids, version and the choice", async () => {
  const { TABS } = await import("../src/screens/project/index.tsx");
  assert.deepEqual(TABS.map((tab) => tab.id), ["overview", "conversation", "agents", "changes", "questions", "context", "process", "metrics", "settings"]);
  const source = readFileSync(new URL("../src/api-questions.ts", import.meta.url), "utf8");
  assert.match(source, /call<void>\("desktop_answer_question", \{ id, version, option, text \}\)/);
  assert.match(source, /call<void>\("desktop_dismiss_question", \{ id, version \}\)/);
  assert.match(source, /call<QuestionPage>\("desktop_questions", \{ projectId, after \}\)/);
});

const { QuestionRow } = await import("../src/screens/team/questions.tsx");

const forbidden = () => { throw new Error("static rendering must not open or answer"); };
const agentQuestion = (id, asked_for, for_label) => ({
  id, agent: id, label: `Agent ${id}`, severity: "normal", asked_for, for_label,
  text: "May I delete the old fixtures?", assumption: null, created_ms: 1, terminal: null,
});
const agentRow = (item) => renderToStaticMarkup(createElement(QuestionRow, { projectId: "project-a", question: item, onOpen: forbidden, onAnswered: forbidden }));

test("each question row says whom its asker meant it for (OP-50)", () => {
  assert.match(agentRow(agentQuestion(1, "me", "Me")), /For: Me</);
  assert.match(agentRow(agentQuestion(2, "master", "Master")), /For: Master</);
  const lead = agentRow(agentQuestion(3, "lead", "Ada (@lead-graph)"));
  assert.match(lead, /For: Ada \(@lead-graph\)</);
  assert.doesNotMatch(lead, /For: Me</);
});
