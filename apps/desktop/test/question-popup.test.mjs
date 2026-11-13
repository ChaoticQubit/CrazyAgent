import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { bundle, withPage } from "./browser.mjs";
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
const FIXTURE = new URL("./fixtures/question-dock.mjs", import.meta.url).pathname;
const view = (extra = {}) => renderToStaticMarkup(createElement(popup.QuestionDock, {
  waiting: [question(), question({ id: 22, question: "Should checkout keep a guest option?", created_ms: NOW - 1_000 })],
  shown: true, now: NOW, busy: false, note: "", settled: 0, onAnswer: () => {}, onDismiss: () => {}, onOpenTab: () => {},
  ...extra,
}));

async function typeText(send, text) {
  for (const key of text) {
    await send("Input.dispatchKeyEvent", { type: "keyDown", key, text: key });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key });
  }
}

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
  assert.match(markup, /<section id="([^"]+)" class="qp-popup" aria-labelledby="\1-title">/);
  assert.match(markup, /<h2 id="[^"]+-title" class="qp-title" tabindex="-1">Question 1 of 2<\/h2>/);
  assert.match(markup, /aria-label="Question from Master"/);
  assert.match(markup, />Which database should the shop use\?</);
  assert.ok(!markup.includes("guest option"), "only the first question is shown");
  for (const word of [">Recommended<", ">Answer<", ">Dismiss<", ">Hide<", ">Questions tab<"]) assert.ok(markup.includes(word), `the pop-up shows ${word}`);
  const card = /<section id="([^"]+)"/.exec(markup)[1];
  assert.match(markup, new RegExp(`aria-expanded="true" aria-controls="${card}"[^>]*>Hide<`), "Hide names the card it folds");
  assert.match(view({ waiting: [question()] }), />A question for you</);
  assert.match(view({ note: "This question changed. Review it again." }), /role="alert">This question changed\. Review it again\.</);
});

test("a polite live line says how many questions wait and who asked the newest, and only where the pop-up shows", () => {
  assert.equal(popup.dockAnnouncement([]), "");
  assert.equal(popup.dockAnnouncement([question()]), "A question waits for you. The newest is from Master.");
  assert.equal(popup.dockAnnouncement([question({ id: 30, asker: "agent:team-4" }), question({ id: 21 })]), "2 questions wait for you. The newest is from Agent team-4.");
  assert.equal(popup.dockAnnouncement([question({ id: 31, asker: "agent:7", asker_name: "Ada (@menu-prices)" })]), "A question waits for you. The newest is from Ada (@menu-prices).", "the asker's name the engine sent (V241-QUESTION-UX P1)");
  assert.match(view(), /<p class="visually-hidden" role="status">2 questions wait for you\. The newest is from Master\.<\/p>/);
  assert.match(view({ shown: false }), /<div class="qp-dock" hidden=""><p class="visually-hidden" role="status"><\/p>/, "a tab without the pop-up keeps it mounted, hidden and silent");
  assert.equal(view({ waiting: [] }), '<div class="qp-dock"><p class="visually-hidden" role="status"></p></div>', "with none waiting only the empty live line is there");
});

test("the pop-up sits in the page's flow above the tab, never over the terminal, the message box or a toast", () => {
  const css = readFileSync(new URL("../src/screens/project/questions.css", import.meta.url), "utf8");
  const rules = css.split("\n").filter((line) => line.startsWith(".qp-"));
  assert.ok(rules.length > 0);
  for (const rule of rules) assert.ok(!/position:\s*(fixed|absolute|sticky)/.test(rule), `no floating pop-up: ${rule}`);
  assert.match(css, /\.qp-dock\[hidden\], \.qp-popup\[hidden\] \{ display: none; \}/, "hidden stays hidden under display: grid");
  const project = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  const banners = project.slice(project.indexOf('<div className="pj-banners">'), project.indexOf('<div className="pj-tab-panel"'));
  assert.match(banners, /<QuestionPopup data=\{data\} shown=\{popupOn\(questionsShown, active\)\} \/>/, "the pop-up is the last banner, above the tab panel");
});

test("Hide and a half-typed comment survive a tab switch, and the folded button names what it opens", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    await evaluate("Fixture.mount(2)");
    await evaluate("Fixture.openComment()");
    await typeText(send, "Keep");
    await evaluate("Fixture.roundTrip()");
    const typed = await evaluate("Fixture.seen()");
    await evaluate('Fixture.press("Hide")');
    const hidden = { seen: await evaluate("Fixture.seen()"), focus: await evaluate("Fixture.where()") };
    await evaluate("Fixture.roundTrip()");
    const back = await evaluate("Fixture.seen()");
    await evaluate('Fixture.press("2 questions waiting")');
    return { typed, hidden, back, shown: { seen: await evaluate("Fixture.seen()"), focus: await evaluate("Fixture.where()") } };
  });
  assert.equal(seen.typed.comment, "Keep", "the comment survives Overview and back");
  assert.equal(seen.hidden.seen.open, false);
  assert.equal(seen.hidden.focus.words, "2 questions waiting", "Hide moves the keyboard to the button that opens it again");
  assert.equal(seen.back.open, false, "still folded after a tab switch");
  assert.equal(seen.back.show, "2 questions waiting");
  assert.equal(seen.back.controls[0], seen.back.controls[2], "the folded button controls the card");
  assert.equal(seen.shown.seen.open, true);
  assert.equal(seen.shown.seen.comment, "Keep", "the comment survives Hide too");
  assert.equal(seen.shown.focus.tag, "h2", "Show moves the keyboard to the card's title");
});

test("after an answer the keyboard goes to the next card's title, and after the last back to the terminal", { timeout: 120_000 }, async () => {
  const fixture = await bundle(FIXTURE);
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate("Fixture.mount(2)");
    await evaluate("Fixture.focusTerminal()");
    await evaluate("Fixture.pickFirst()");
    await evaluate('Fixture.press("Answer")');
    const next = { seen: await evaluate("Fixture.seen()"), focus: await evaluate("Fixture.where()") };
    await evaluate("Fixture.pickFirst()");
    await evaluate('Fixture.press("Answer")');
    return { next, last: { seen: await evaluate("Fixture.seen()"), focus: await evaluate("Fixture.where()") } };
  });
  assert.deepEqual(seen.next.seen.answered, [1]);
  assert.equal(seen.next.seen.title, "A question for you");
  assert.deepEqual([seen.next.focus.tag, seen.next.focus.words], ["h2", "A question for you"], "the next card takes the keyboard, not the page body");
  assert.equal(seen.next.seen.live, "A question waits for you. The newest is from Master.");
  assert.deepEqual(seen.last.seen.answered, [1, 2]);
  assert.equal(seen.last.focus.id, "terminal", "with none left the keyboard returns to where the owner was");
  assert.equal(seen.last.seen.live, "");
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
  assert.match(project, /questionsShown !== "tab" && <QuestionPopup data=\{data\} shown=\{popupOn\(questionsShown, active\)\} \/>/,
    "the project page keeps the pop-up mounted unless the owner chose Questions tab only, and the tab decides whether it shows");
});
