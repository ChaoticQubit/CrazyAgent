import { act, createElement } from "react";
import { KeyOrUrlForm, methodOptions } from "../../src/screens/settings/add-account.tsx";
import { mountTab, settle, wordsOf } from "./settings-tab.mjs";

export { settle, wordsOf };

const LOCAL = { provider: "local", name: "Local models (Ollama)", methods: [
  { method: "url", label: "Connect a URL", url_preset: "http://127.0.0.1:11434/v1", url: "required", key: "hidden", model: "required" }] };
let waiting = [];

/** Mounts the local model server form against a fake engine whose model lookups wait until the
 * row answers them. Inputs: the scene (unused) and the theme. Output: nothing. */
export async function mount(_scene, theme) {
  waiting = [];
  const answer = (command, args) => {
    if (command !== "desktop_local_models") throw { code: "unexpected_command" };
    return new Promise((resolve, reject) => { waiting.push({ url: args.url, resolve, reject }); });
  };
  const form = createElement(KeyOrUrlForm, { provider: LOCAL, option: methodOptions(LOCAL)[0], name: "Local 1", onName: () => {}, onAdded: () => {} });
  await mountTab(form, answer, theme);
}

/** Types new words into the Base URL field the way an input event delivers them. Input: the
 * words. Output: nothing. */
export async function typeUrl(words) {
  const field = document.getElementById("account-url");
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
  await act(async () => {
    setter.call(field, words);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await settle(30);
}

/** Presses Find models. Output: nothing. */
export async function findModels() {
  const button = [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === "Find models");
  await act(async () => { button.click(); });
  await settle(30);
}

/** Answers the oldest waiting lookup for one URL, with its models or a failure code. Inputs: the
 * URL it asked for and the answer. Output: whether a lookup was waiting. */
export async function answer(url, reply) {
  const at = waiting.findIndex((item) => item.url === url);
  if (at < 0) return false;
  const [item] = waiting.splice(at, 1);
  await act(async () => { if (reply.code) item.reject({ code: reply.code }); else item.resolve(reply.models); });
  await settle(60);
  return true;
}

/** What the form shows about the last lookup: the alert, the empty-result line and the model
 * choices. Output: those words and the choices. */
export function shown() {
  const select = document.querySelector("select#account-model");
  return {
    alert: wordsOf('#mount [role="alert"]'),
    empty: wordsOf(".row .secondary"),
    choices: select ? [...select.options].map((option) => option.value) : [],
  };
}
