import { Fragment, createElement } from "react";
import { Settings } from "../../src/screens/settings/index.tsx";
import { loadTerminalKind } from "../../src/screens/team/terminal-kind.ts";
import { UserMenuDialog } from "../../src/screens/user-menu.tsx";
import { isUserMenuTab } from "../../src/ui/router.ts";
import { RULE_LABELS } from "../../src/screens/project/publish-job.ts";
import { mountTab, sceneRead } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const FAILURE = { code: "engine_failed" };
const account = (index, patch = {}) => ({ id: `acct-${index}`, name: `Account ${index}`, provider: "chatgpt", provider_name: "ChatGPT / Codex", method: "subscription", method_label: "Sign in with ChatGPT",
  url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });
const ACCOUNTS = {
  ok: [account(0), account(1, { provider: "openai_compatible", provider_name: "Any OpenAI-compatible URL", method: "url", method_label: "Connect a URL" }),
    account(2, { provider: "claude", provider_name: "Claude", method_label: "Sign in with your Claude subscription" })],
  empty: [],
  many: Array.from({ length: 30 }, (_, index) => account(index)),
  long_status: [account(0, { status_label: "Can't reach this URL" })],
};
const SIGNED_IN = { signed_in: true, login: "mira", method: "device", waiting: false, problem: null };
const GITHUB = { in: SIGNED_IN, out: { signed_in: false, login: null, method: "none", waiting: false, problem: null }, cli: { ...SIGNED_IN, method: "cli" }, trouble: { ...SIGNED_IN, problem: "The sign-in expired." } };
const STANDARD = Object.keys(RULE_LABELS).filter((rule) => rule !== "signed_commits");
let defaultsSaved = null;
const OWNER = { name: "Mira", role: "Founder", bio: "Builds things.", preferred_stack: ["Rust"], working_style: { detail: "terse", initiative: "ask_first" } };
const PROFILES = { ok: { kind: "ok", value: OWNER }, blank: { kind: "ok", value: { ...OWNER, name: "" } }, loading: { kind: "loading" }, not_ready: { kind: "not_ready" }, error: { kind: "error", message: "engine_failed" } };

/** Answers one engine command for a scene. Inputs: the scene and the command. Output: the
 * answer, a failure thrown, or a read that never lands. */
function answer(scene, command, args) {
  if (command === "desktop_list_accounts") {
    const accounts = sceneRead(scene.accounts ?? "ok", ACCOUNTS, FAILURE, command);
    return accounts instanceof Promise ? accounts : { accounts, next: null };
  }
  if (command === "desktop_pool_summaries") return [];
  if (command === "desktop_github_status") return sceneRead(scene.github ?? "in", GITHUB, FAILURE, command);
  if (command === "desktop_github_sign_out") return sceneRead(scene.signOut ?? "out", GITHUB, FAILURE, command);
  if (command === "desktop_github_rule_defaults") {
    if (scene.defaults === "unreadable") throw { code: "github_state_invalid" };
    return sceneRead(scene.defaults ?? "ok", { ok: defaultsSaved ?? STANDARD }, FAILURE, command);
  }
  if (command === "desktop_github_rule_defaults_set") {
    defaultsSaved = [...args.rules];
    return defaultsSaved;
  }
  if (command === "desktop_save_owner_profile") return sceneRead(scene.save ?? "ok", { ok: null }, FAILURE, command);
  if (command === "desktop_remove_account" || command === "desktop_account_catalog") throw FAILURE;
  throw `Command ${command} not found`;
}

/** Mounts a Settings page section, or GitHub or Profile in its user menu dialog (DEC-151),
 * or a user menu dialog (`over`) open over a Settings section as the app keeps the page under
 * it, against a fake engine. Inputs: the scene, naming the tab, the route action, `over`, each
 * read and `native` (a build that draws every terminal natively), and the theme. Output: nothing. */
export async function mount(scene, theme = "light") {
  const session = { profile: PROFILES[scene.profile ?? "ok"], refreshProfile: async () => {} };
  const dialog = (tab) => createElement(UserMenuDialog, { tab, onClose: () => {} });
  const page = () => createElement(Settings, { tab: scene.tab, action: scene.action });
  const panel = isUserMenuTab(scene.tab) ? dialog(scene.tab) : scene.over ? createElement(Fragment, null, page(), dialog(scene.over)) : page();
  defaultsSaved = null;
  await loadTerminalKind(async () => ({ kind: "ok", value: scene.native === true }));
  await mountTab(panel, (command, args) => answer(scene, command, args), theme, session);
}

/** The rules the last defaults save sent. Output: the list, or null before any save. */
export function defaultsSent() {
  return defaultsSaved;
}
