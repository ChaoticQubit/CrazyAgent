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
  id: 12, project_id: "p-1", asker: "agent:team-3", question: "Which database should the app use?", kind: "single", options: ["Postgres", "SQLite"],
  recommended: "SQLite", context: "It runs on one laptop.", status: "pending", version: 3, answer: null, created_ms: NOW - 120_000, settled_ms: null,
  ...extra,
});
const COMMENT_AFTER_THE_GROUP = /<\/fieldset><label class="qs-option qs-comment"><input type="checkbox"[^>]*\/><span>Add a comment<\/span><\/label>/;
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
  assert.match(markup, /<legend[^>]*>Pick one<\/legend>/);
  assert.doesNotMatch(markup, /<textarea/, "the comment box opens only once Add a comment is picked");
  assert.match(markup, /<button[^>]*disabled=""[^>]*>Answer<\/button>/, "Answer waits for a pick or a comment");
  assert.match(markup, /<button[^>]*>Dismiss<\/button>/);
  assert.doesNotMatch(markup, /<button[^>]*disabled=""[^>]*>Dismiss<\/button>/);
  assert.equal(client.isRecommended(question(), 1), true);
  assert.equal(client.isRecommended(question(), 0), false);
  assert.equal(client.isRecommended(question({ recommended: null }), 1), false);
  const busy = card(question(), { busy: true });
  assert.match(busy, /<button[^>]*disabled=""[^>]*>Dismiss<\/button>/);
});

test("a question with no options offers its suggested answer and a field of its own", () => {
  const markup = card(question({ kind: "text", options: [], recommended: "Keep it private" }));
  assert.match(markup, />Suggested: Keep it private</);
  assert.match(markup, />Use suggested answer</);
  assert.match(markup, /<label for="[^"]+">Your answer<\/label>/);
  assert.doesNotMatch(markup, /type="radio"/);
  assert.doesNotMatch(card(question({ kind: "text", options: [], recommended: null })), /Suggested/);
});

test("a multi question shows checkboxes and Add a comment last", () => {
  const html = card(question({ kind: "multi", options: ["Web", "Desktop"], recommended: "Web" }));
  assert.equal((html.match(/type="checkbox"/g) ?? []).length, 3);
  assert.doesNotMatch(html, /type="radio"/);
  assert.match(html, /<legend[^>]*>Pick any<\/legend>/);
  assert.match(html, COMMENT_AFTER_THE_GROUP);
  assert.equal(html.match(/>Recommended</g)?.length, 1);
});

test("a single question shows radios and Add a comment last", () => {
  const html = card(question({ kind: "single" }));
  assert.equal((html.match(/type="radio"/g) ?? []).length, 2);
  assert.equal((html.match(/type="checkbox"/g) ?? []).length, 1);
  assert.match(html, COMMENT_AFTER_THE_GROUP);
  assert.equal(client.COMMENT_LABEL, "Add a comment");
});

test("Add a comment is a checkbox outside the option group that says whether its box is open, and opening moves the keyboard into the box", () => {
  for (const kind of ["single", "multi"]) {
    const html = card(question({ kind }));
    const group = html.slice(html.indexOf("<fieldset"), html.indexOf("</fieldset>"));
    assert.doesNotMatch(group, /Add a comment/, kind);
    assert.match(html, /<input type="checkbox" aria-expanded="false"\/><span>Add a comment<\/span>/, kind);
    assert.doesNotMatch(html, /<textarea/, kind);
  }
  const source = readFileSync(new URL("../src/screens/project/questions.tsx", import.meta.url), "utf8");
  assert.match(source, /useEffect\(\(\) => \{\s*if \(commenting\) document\.getElementById\(field\)\?\.focus\(\);\s*\}, \[commenting, field\]\);/);
  assert.match(source, /aria-controls=\{commenting \? field : undefined\}/);
});

test("a text question shows only the text box", () => {
  const html = card(question({ kind: "text", options: [], recommended: "Call it Atlas" }));
  assert.doesNotMatch(html, /type="radio"|type="checkbox"|Add a comment/);
  assert.match(html, /<textarea/);
});

test("picking replaces the pick of a single question, adds to a multi one, and a second click clears it", () => {
  assert.deepEqual(view.picksAfter("single", [], 1), [1]);
  assert.deepEqual(view.picksAfter("single", [1], 0), [0]);
  assert.deepEqual(view.picksAfter("single", [1], 1), []);
  assert.deepEqual(view.picksAfter("multi", [2], 0), [0, 2]);
  assert.deepEqual(view.picksAfter("multi", [0, 2], 2), [0]);
  assert.deepEqual(view.picksAfter("text", [], 0), []);
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

test("the choice sent holds the picks in order and a trimmed comment, or nothing to send", () => {
  assert.deepEqual(view.choiceOf("multi", [2, 0], "  later "), { picks: [0, 2], comment: "later" });
  assert.deepEqual(view.choiceOf("single", [], "Neither"), { picks: [], comment: "Neither" });
  assert.deepEqual(view.choiceOf("single", [1], null), { picks: [1], comment: null });
  assert.deepEqual(view.choiceOf("single", [1], " \n\t "), { picks: [1], comment: null });
  assert.deepEqual(view.choiceOf("single", [0], "Ship the web first."), { picks: [0], comment: "Ship the web first." });
  assert.deepEqual(view.choiceOf("text", [], "  Use SQLite  "), { picks: [], comment: "Use SQLite" });
  assert.equal(view.choiceOf("single", [], "   "), null);
  assert.equal(view.choiceOf("single", [], null), null);
  assert.equal(view.choiceOf("single", [0, 1], null), null);
  assert.equal(view.choiceOf("text", [0], "Atlas"), null);
  assert.equal(view.choiceOf("multi", [client.OPTIONS_MAX], null), null);
  assert.equal(view.choiceOf("multi", [-1], null), null);
  assert.equal(view.choiceOf("multi", [0.5], null), null);
  assert.deepEqual(view.choiceOf("multi", [0], "a".repeat(client.COMMENT_BYTES_MAX)), { picks: [0], comment: "a".repeat(client.COMMENT_BYTES_MAX) });
  assert.equal(view.choiceOf("multi", [0], "a".repeat(client.COMMENT_BYTES_MAX + 1)), null, "a comment with picks is at most 2048 bytes");
  assert.equal(view.choiceOf("multi", [0], "é".repeat(client.COMMENT_BYTES_MAX / 2 + 1)), null, "the bound is bytes, not characters");
  assert.deepEqual(view.choiceOf("single", [], "a".repeat(client.ANSWER_BYTES_MAX)), { picks: [], comment: "a".repeat(client.ANSWER_BYTES_MAX) });
  assert.equal(view.choiceOf("single", [], "a".repeat(client.ANSWER_BYTES_MAX + 1)), null);
  assert.equal(client.COMMENT_BYTES_MAX, 2048);
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

test("a question the master closed says so: withdrawn, asked again as a choice, or answered in its terminal (F12, F14)", () => {
  assert.deepEqual(client.statusBadge("dismissed", "master"), { word: "Withdrawn by the master", tone: "neutral" });
  assert.deepEqual(client.statusBadge("dismissed", "replaced"), { word: "Asked again as a choice", tone: "neutral" });
  assert.deepEqual(client.statusBadge("answered_in_terminal", "relay"), { word: "Answered in the master's terminal", tone: "ok" });
  assert.deepEqual(client.statusBadge("pending", "master"), { word: "Waiting for you", tone: "warn" }, "a waiting question is never shown as closed");
  assert.deepEqual(client.statusBadge("dismissed", null), { word: "Dismissed", tone: "neutral" });
  const withdrawn = card(question({ status: "dismissed", closed_by: "master", settled_ms: NOW }));
  assert.match(withdrawn, />Withdrawn by the master</);
  assert.doesNotMatch(withdrawn, />Dismiss</);
  const relayed = card(question({ status: "answered_in_terminal", closed_by: "relay", answer: "Use SQLite.", settled_ms: NOW }));
  assert.match(relayed, />Answered in the master&#x27;s terminal</);
  assert.match(relayed, /<span class="pj-secondary">The master reports: <\/span>Use SQLite\./);
  assert.match(card(question({ status: "dismissed", closed_by: "replaced", settled_ms: NOW })), />Asked again as a choice</);
});

test("Dismiss all sends the waiting questions shown, oldest first, at most one page, and says what happened to each", async () => {
  const shown = [
    question({ id: 1, status: "answered", answer: "Yes", settled_ms: NOW }),
    ...Array.from({ length: client.BULK_DISMISS_MAX + 3 }, (_, at) => question({ id: 100 + at, version: at, created_ms: NOW - 1000 + at })),
  ];
  const waiting = view.dismissAllOf(view.mergedQuestions({ items: shown, next: null }, []));
  assert.equal(waiting.length, client.BULK_DISMISS_MAX);
  assert.equal(client.BULK_DISMISS_MAX, 50);
  assert.ok(waiting.every((item) => item.status === "pending"));
  assert.deepEqual(waiting.slice(0, 2).map((item) => item.id), [100, 101]);
  assert.deepEqual(view.dismissAllOf([question({ status: "answered" })]), []);
  assert.equal(client.bulkDismissedLine({ settled: [1, 2], stale: [], refused: [] }), "Dismissed 2 questions.");
  assert.equal(
    client.bulkDismissedLine({ settled: [1], stale: [3], refused: [{ id: 4, code: "questions_delivery_full" }] }),
    "Dismissed 1 question. 1 changed since you read them. Review them again. 1 could not be dismissed: The master's conversation is full right now. Try again after it reads its messages.",
  );
  assert.match(client.bulkDismissNote(3), /^All 3 waiting questions leave the list\. Each agent that asked is told once/);
  await assert.rejects(async () => client.dismissQuestions([]), /1 to 50 questions/);
  const source = readFileSync(new URL("../src/api-questions.ts", import.meta.url), "utf8");
  assert.match(source, /call<QuestionsDismissed>\("desktop_dismiss_questions", \{ items: questions\.map\(\(question\) => \[question\.id, question\.version\]\) \}\)/);
  const tab = readFileSync(new URL("../src/screens/project/questions.tsx", import.meta.url), "utf8");
  assert.match(tab, /waiting\.length >= 2 && /);
  assert.match(tab, /<ConfirmDialog open=\{dismissingAll !== null\}/);
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
  const askSide = ["questions_ask_invalid", "questions_asker_full", "questions_full", "questions_not_terminal", "questions_notes_invalid", "questions_owner_not_typed", "questions_owner_turn_invalid", "questions_owner_turn_used", "questions_page_unavailable", "questions_replace_invalid"];
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
  const linked = client.unseenQuestionAlerts([{ project_id: "p-7", unseen: 1, questions: 1 }]);
  assert.deepEqual([...client.withPendingQuestions(new Map([["p-7", 1]]), linked, new Map([["p-7", 1]]))], [["p-7", 1]], "a team question linked into the questions log, its alert unseen, counts once (F12)");
});

test("the Questions tab sits after Changes and the commands go through the bridge with only ids, version and the choice", async () => {
  const { TABS } = await import("../src/screens/project/index.tsx");
  assert.deepEqual(TABS.map((tab) => tab.id), ["overview", "conversation", "agents", "changes", "questions", "context", "process", "metrics", "settings"]);
  const source = readFileSync(new URL("../src/api-questions.ts", import.meta.url), "utf8");
  assert.match(source, /call<void>\("desktop_answer_question", \{ id, version, picks: choice\.picks, comment: choice\.comment \}\)/);
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

test("the Agents toolbar says how many ask the owner, and the drawer counts the open questions in its title", async () => {
  const inbox = await import("../src/screens/team/questions.tsx");
  const read = (questions, more, waiting) => ({ project_id: "p", questions: Array.from({ length: questions }, (_, index) => question({ id: index + 1 })), more, waiting: Array.from({ length: waiting }, (_, index) => ({ agent: index + 1, label: `A${index}`, waiting: "question", since_ms: 0, terminal: null })) });
  assert.equal(inbox.askCount(read(1, 2, 1)), 4);
  assert.equal(inbox.asksWords(inbox.askCount(read(0, 0, 1))), "1 asks you");
  assert.equal(inbox.asksWords(3), "3 ask you");
  assert.equal(inbox.asksWords(0), "no one asks you");
  assert.throws(() => inbox.asksWords(1.5), /whole number/);
  assert.equal(inbox.questionsTitle({ shown: { kind: "ok", value: read(0, 0, 2) }, stale: null }), "Questions for you");
  assert.equal(inbox.questionsTitle({ shown: { kind: "ok", value: read(1, 1, 0) }, stale: null }), "Questions for you (2)");
  assert.equal(inbox.questionsTitle({ shown: undefined, stale: null }), "Questions for you");
});

test("a row read later takes the keyboard only while the owner has not put it anywhere in the drawer (MOCKB2-BUTTON-FOCUS)", async () => {
  const panel = await import("../src/screens/team/questions.tsx");
  assert.equal(typeof panel.keyboardFree, "function", "the panel decides when a row may take the keyboard");
  const doc = {};
  const body = { ownerDocument: doc, closest: () => null };
  doc.body = body;
  const drawer = { ownerDocument: doc, contains: (node) => node.inside === true, closest: () => null };
  const inDrawer = (head) => ({ ownerDocument: doc, inside: true, closest: (selector) => head && selector === ".dialog-head" ? drawer : null });
  assert.equal(panel.keyboardFree(null, drawer), true, "nothing focused");
  assert.equal(panel.keyboardFree(body, drawer), true, "the page");
  assert.equal(panel.keyboardFree(drawer, drawer), true, "the drawer itself");
  assert.equal(panel.keyboardFree(inDrawer(true), drawer), true, "the drawer's Close button, where a drawer with no field starts");
  assert.equal(panel.keyboardFree(inDrawer(false), drawer), false, "a button the owner moved to, like Send answer");
  assert.equal(panel.keyboardFree({ ...inDrawer(true), inside: false }, drawer), false, "another dialog's head");
  assert.equal(panel.keyboardFree(inDrawer(false), null), false, "no drawer, a control keeps its focus");
});
