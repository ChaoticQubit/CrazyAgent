import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { RecountPanel } from "../../src/screens/metrics/view.tsx";

const CHOICES = ["Use new numbers", "Keep old numbers"];
const tokens = (input) => ({ input, output: 0, cache_read: 0, cache_write: 0 });
const STATUS = { old: tokens(400), new: tokens(300), new_cost_nanos: 0, sessions_recounted: 2, sessions_kept_old: 1, homes_partial: 0 };
const MONEY = { settings: { main_number: "tokens", prices: "off" }, shown: "tokens", as_of: null, cost_nanos: 0, unpriced_tokens: 0 };
let root = null;
let choices = [];
let answers = [];
let done = 0;
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/** Sends a recount choice the way the engine call does, answering only when the test replies.
 * Input: whether to use the new numbers. Output: the result, once replied. */
function decide(useNew) {
  choices.push(useNew);
  return new Promise((answer) => answers.push(answer));
}

async function acted(work) {
  await act(async () => {
    work();
    await new Promise((settled) => setTimeout(settled, 0));
  });
}

function state() {
  return {
    choices: [...choices],
    waiting: answers.length,
    done,
    off: [...document.querySelectorAll("button")].filter((button) => CHOICES.includes(button.textContent)).map((button) => button.disabled),
    alert: document.querySelector("[role=alert]")?.textContent ?? null,
  };
}

/** Mounts the recount offer with the test's engine call. Output: what it shows. */
export async function mount() {
  root?.unmount();
  choices = [];
  answers = [];
  done = 0;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  await acted(() => root.render(createElement(RecountPanel, { status: STATUS, money: MONEY, onDone: () => { done += 1; }, decide })));
  return state();
}

/** Clicks the button with these words, even a disabled one, as a second click would. Input:
 * the words. Output: the state, with missed set when no such button shows. */
export async function click(words) {
  const target = [...document.querySelectorAll("button")].find((button) => button.textContent === words);
  if (!target) return { ...state(), missed: words };
  await acted(() => target.click());
  return { ...state(), missed: null };
}

/** Answers the oldest choice still waiting. Input: the engine's result. Output: the state. */
export async function answer(result) {
  const reply = answers.shift();
  if (!reply) throw new Error("no recount choice is waiting for a reply");
  await acted(() => reply(result));
  return state();
}
