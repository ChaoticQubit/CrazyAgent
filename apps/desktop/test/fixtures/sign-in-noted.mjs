import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { SessionContext } from "../../src/state.ts";
import { AccountDrawer } from "../../src/screens/settings/account-drawer.tsx";

const ACCOUNT = {
  id: "acct-1", name: "Claude one", provider: "claude", provider_name: "Claude", method: "subscription",
  method_label: "Sign in with your Claude subscription", url: null, model: null, status: "not_signed_in", status_label: "Not signed in",
};
const RUNNING = { ...ACCOUNT, id: "acct-2", name: "Claude two", status: "signing_in", status_label: "Signing in" };
const DETAIL = {
  ticket: "ticket-1", collection: "connections", revision: "1", config_id: "config-1", source_hash: "source-1", writable: true, fresh: true,
  record: { kind: "connection", connection: {
    id: "acct-1", provider: "provider-claude", harness: "claude", protocol: "native", account_ref: "account-1",
    endpoint: { kind: "native" }, model: { id: "model-1", author: "author-1" }, auth: { method: "native_managed", profile_ref: "profile-1" },
  } },
};

let root = null;
const noted = [];

/** Mounts the real account drawer with Sign in again open, under a session that records every
 * result it is asked to note, with the bridge refusing a new sign-in with the given code.
 * Inputs: the refusal code and, for a sign-in that still runs, the id of its account. */
export function mount(refusal, account) {
  root?.unmount();
  noted.length = 0;
  window.__TAURI_INTERNALS__ = {
    invoke: async (command) => {
      if (command === "desktop_inspect_model_access") return DETAIL;
      if (command === "desktop_list_accounts") return { accounts: [ACCOUNT, RUNNING], next: null };
      if (command === "desktop_start_native_auth") throw account ? { code: refusal, account } : { code: refusal, message: refusal };
      throw { code: "unknown", message: command };
    },
    transformCallback: () => 0,
  };
  const session = {
    engine: "running", unconfirmed: null, profileCheck: null, stuckSignIn: null, profile: { kind: "ok", value: null },
    projects: { kind: "ok", value: [] }, projectsMore: false, toast: "", toastOpen: null, revision: 1,
    retry() {}, checkUnconfirmed: async () => {}, checkProfile: async () => {}, clearStuckSignIn: async () => {},
    retryUnconfirmed: async () => {}, dropUnconfirmed: async () => {}, refreshProfile: async () => {}, refreshProjects: async () => {},
    showToast() {}, noteResult: (result) => { noted.push(result); return result; },
  };
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(h(SessionContext.Provider, { value: session },
    h(AccountDrawer, { account: ACCOUNT, pools: [], signIn: true, onClose() {}, onChanged() {}, onDelete() {} }))));
}

/** Waits up to a bound for a condition on the page. Inputs: the condition and the bound in ms.
 * Output: true once it holds, false at the bound. */
async function until(condition, ms) {
  for (const end = Date.now() + ms; Date.now() < end; await new Promise((done) => setTimeout(done, 50))) {
    if (condition()) return true;
  }
  return condition();
}

const button = (label) => [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === label);

/** Starts the Claude sign-in from the drawer and waits for the panel's failure. Output: the
 * panel offered Open Claude sign-in, the codes the session noted and the drawer's text. */
export async function startSignIn() {
  const offered = await until(() => Boolean(button("Open Claude sign-in")) && !button("Open Claude sign-in").disabled, 5000);
  if (offered) button("Open Claude sign-in").click();
  await until(() => Boolean(document.querySelector("[role=\"alert\"]")), 5000);
  return { offered, noted: noted.map((result) => result.kind === "error" ? result.code : result.kind), text: document.body.innerText };
}
