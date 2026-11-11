import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-questions.ts");
const popup = await import("../src/screens/project/question-popup.tsx");
const state = await import("../src/state.ts");
const page = await import("../src/screens/project/index.tsx");

const NOW = Date.UTC(2026, 10, 11, 13, 0);
const question = (extra = {}) => ({
  id: 21, project_id: "p-1", asker: "master:s-1", question: "Which database should the shop use?", kind: "single", options: ["Postgres", "SQLite"],
  recommended: "Postgres", context: null, status: "pending", version: 0, answer: null, created_ms: NOW - 60_000, settled_ms: null,
  ...extra,
});
const TABS = [...page.TABS.map((tab) => tab.id), "ide"];
const view = (extra = {}) => renderToStaticMarkup(createElement(popup.QuestionPopupView, {
  waiting: [question(), question({ id: 22, question: "Should checkout keep a guest option?", created_ms: NOW - 1_000 })],
  open: true, now: NOW, busy: false, note: "", onAnswer: () => {}, onDismiss: () => {}, onHide: () => {}, onShow: () => {}, onOpenTab: () => {},
  ...extra,
}));

/** Puts a storage the test reads back on `window`, as the app's webview has one. Output: the
 * kept entries. */
function storage() {
  const kept = new Map();
  globalThis.window = { localStorage: { getItem: (key) => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, String(value)), removeItem: (key) => kept.delete(key) } };
  return kept;
}

test("Questions show offers every option, the Conversation tab pop-up by default, and reads anything else as the default", () => {
  assert.deepEqual(client.QUESTIONS_SHOWN_CHOICES.map((choice) => choice.value), ["conversation", "every", "tab"]);
  assert.equal(client.QUESTIONS_SHOWN_DEFAULT, "conversation");
  for (const choice of ["conversation", "every", "tab"]) assert.equal(client.questionsShown(choice), choice);
  for (const stored of [null, "", "popup", "Conversation"]) assert.equal(client.questionsShown(stored), "conversation", `${stored} reads as the default`);
});

test("option Pop-up on Conversation: the pop-up shows on the Conversation tab only", () => {
  assert.deepEqual(TABS.filter((tab) => popup.popupOn("conversation", tab)), ["conversation"]);
});

test("option Pop-up on every tab: the pop-up shows on every project tab but Questions, which lists them itself", () => {
  assert.deepEqual(TABS.filter((tab) => popup.popupOn("every", tab)), TABS.filter((tab) => tab !== "questions"));
});

test("option Questions tab only: no tab shows the pop-up", () => {
  assert.deepEqual(TABS.filter((tab) => popup.popupOn("tab", tab)), []);
});

test("the choice is kept in this app and read back, and a missing storage reads as the default", () => {
  const kept = storage();
  try {
    assert.equal(state.readQuestionsShown(), "conversation");
    state.saveQuestionsShown("tab");
    assert.deepEqual([...kept.entries()], [["crazyagent.questions-shown", "tab"]]);
    assert.equal(state.readQuestionsShown(), "tab");
    globalThis.window = { get localStorage() { throw new Error("storage is off"); } };
    assert.equal(state.readQuestionsShown(), "conversation");
    assert.doesNotThrow(() => state.saveQuestionsShown("every"));
  } finally {
    delete globalThis.window;
  }
});

test("the pop-up offers only waiting questions, the one waiting longest first", () => {
  const waiting = popup.popupQuestions([
    question({ id: 1, status: "answered", answer: "Yes", settled_ms: NOW - 5 }),
    question({ id: 2, created_ms: NOW - 1_000 }),
    question({ id: 3, status: "dismissed", settled_ms: NOW - 3 }),
    question({ id: 4, created_ms: NOW - 9_000 }),
  ]);
  assert.deepEqual(waiting.map((item) => item.id), [4, 2]);
});

test("Hide folds the pop-up until a newer question arrives", () => {
  const waiting = [question({ id: 4 }), question({ id: 7 })];
  assert.equal(popup.popupOpen(waiting, 0), true);
  assert.equal(popup.popupOpen(waiting, 7), false, "hidden after the newest one was seen");
  assert.equal(popup.popupOpen([...waiting, question({ id: 9 })], 7), true, "a newer question opens it again");
  assert.equal(popup.popupOpen([], 0), false);
});

test("the open pop-up shows the first waiting question as the Questions tab card, with Answer, Dismiss and Hide", () => {
  const markup = view();
  assert.match(markup, /<section class="qp-dock qp-popup" aria-label="Questions waiting for you">/);
  assert.match(markup, />Question 1 of 2</);
  assert.match(markup, /aria-label="Question from Master"/);
  assert.match(markup, />Which database should the shop use\?</);
  assert.ok(!markup.includes("guest option"), "only the first question is shown");
  for (const word of [">Recommended<", ">Answer<", ">Dismiss<", ">Hide<", ">Questions tab<"]) assert.ok(markup.includes(word), `the pop-up shows ${word}`);
  assert.match(view({ waiting: [question()] }), />A question for you</);
  assert.match(view({ note: "This question changed. Review it again." }), /role="alert">This question changed\. Review it again\.</);
});

test("a hidden pop-up keeps one button that counts the waiting questions, and nothing shows when none waits", () => {
  const folded = view({ open: false });
  assert.match(folded, /<div class="qp-dock">/);
  assert.match(folded, /aria-expanded="false"[^>]*>.*2 questions waiting/);
  assert.ok(!folded.includes("Which database"), "the card is folded away");
  assert.match(view({ open: false, waiting: [question()] }), /1 question waiting/);
  assert.equal(view({ waiting: [] }), "");
});

test("the pop-up and the Questions tab answer through one path: the same read, answer and dismiss commands", () => {
  const source = readFileSync(new URL("../src/screens/project/question-popup.tsx", import.meta.url), "utf8");
  const tab = readFileSync(new URL("../src/screens/project/questions.tsx", import.meta.url), "utf8");
  assert.match(source, /loadQuestions\(/, "the pop-up reads the Questions list");
  assert.match(source, /useQuestionActions\(/, "the pop-up settles with the Questions tab's actions");
  assert.ok(!/\bcall\(|answerQuestion\(|dismissQuestion\(/.test(source), "the pop-up has no answer path of its own");
  assert.match(tab, /export function useQuestionActions\(/);
  assert.match(tab, /answerQuestion\(question\.id, question\.version, choice\)/);
  assert.match(tab, /dismissQuestion\(question\.id, question\.version\)/);
  assert.equal((tab.match(/useQuestionActions\(/g) ?? []).length, 2, "the Questions tab uses the same actions");
});

test("Settings, Profile shows Questions show with all three options and the one chosen", () => {
  const markup = renderToStaticMarkup(createElement(popup.QuestionsShownSetting, { value: "every", onValue: () => {} }));
  assert.match(markup, /role="radiogroup" aria-label="Questions show"/);
  const options = [...markup.matchAll(/role="radio" aria-checked="(true|false)"[^>]*>(?:<[^>]+>)*([^<]+)</g)].map((match) => [match[2].trim(), match[1]]);
  assert.deepEqual(options, [["Pop-up on Conversation", "false"], ["Pop-up on every tab", "true"], ["Questions tab only", "false"]]);
  const profile = readFileSync(new URL("../src/screens/settings/profile.tsx", import.meta.url), "utf8");
  assert.match(profile, /<QuestionsShownSetting /, "the setting sits in Settings, Profile");
  const project = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(project, /popupOn\(questionsShown, active\) && <QuestionPopup /, "the project page shows the pop-up as the setting says");
});
