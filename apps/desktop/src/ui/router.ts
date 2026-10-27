import { useEffect, useState } from "react";

export const SETTINGS_TABS = ["accounts", "pools", "harnesses", "plugins", "process", "pack", "home", "github", "memory", "usage", "profile", "updates"] as const;
export const PROJECT_TABS = ["overview", "conversation", "agents", "changes", "context", "process", "settings"] as const;
export const NEW_PROJECT_STEPS = ["basics", "documents", "work", "pool", "team"] as const;
const SETTINGS_ACTIONS = ["add", "new", "sign_in"] as const;
export const ADD_PROJECT_WAYS = ["clone", "folder"] as const;
const PROJECT_ID_MAX = 256;
const ACCOUNT_ID = /^[A-Za-z0-9_.:-]{1,128}$/;
const SKILL_ID = /^[a-z0-9-]{1,64}$/;

export type SettingsTab = typeof SETTINGS_TABS[number];
export type ProjectTab = typeof PROJECT_TABS[number];
export type NewProjectStep = typeof NEW_PROJECT_STEPS[number];
export type SettingsAction = typeof SETTINGS_ACTIONS[number];
export type AddProjectWay = typeof ADD_PROJECT_WAYS[number];

export type Route =
  | { name: "welcome" }
  | { name: "home" }
  | { name: "new-project"; step: NewProjectStep }
  | { name: "project"; id: string; tab: ProjectTab }
  | { name: "add-project"; way: AddProjectWay }
  | { name: "onboarding"; id: string }
  | { name: "settings"; tab: SettingsTab; action?: SettingsAction; account?: string; skill?: string };

function pick<T extends string>(choices: readonly T[], value: string | null | undefined, fallback: T): T {
  return choices.find((choice) => choice === value) ?? fallback;
}

/** Reads the app's hash route. Unknown or malformed routes go Home; a missing tab or step
 * goes to the first one; Sign in again keeps its account only on Accounts and only when the id
 * has the engine's account id shape; an open skill stays only on Plugins and skills and only with
 * the engine's skill id shape. Input: a location hash such as "#/settings/pools".
 * Output: a route. */
export function parseRoute(hash: string): Route {
  const [path, query = ""] = hash.replace(/^#/, "").split("?", 2);
  const parts = path.split("/").filter(Boolean);
  const params = new URLSearchParams(query);
  if (parts[0] === "welcome" && parts.length === 1) return { name: "welcome" };
  if (parts[0] === "settings" && parts.length <= 2) {
    const tab = pick(SETTINGS_TABS, parts[1], "accounts");
    const action = SETTINGS_ACTIONS.find((choice) => choice === params.get("action"));
    const skill = params.get("skill") ?? "";
    if (!action && tab === "plugins" && SKILL_ID.test(skill)) return { name: "settings", tab, skill };
    if (action !== "sign_in") return action ? { name: "settings", tab, action } : { name: "settings", tab };
    const account = params.get("account") ?? "";
    return tab === "accounts" && ACCOUNT_ID.test(account) ? { name: "settings", tab, action, account } : { name: "settings", tab };
  }
  if (parts[0] === "projects" && parts[1] === "new" && parts.length === 2) {
    return { name: "new-project", step: pick(NEW_PROJECT_STEPS, params.get("step"), "basics") };
  }
  if (parts[0] === "projects" && parts[1] === "add" && parts.length === 3) {
    const way = ADD_PROJECT_WAYS.find((choice) => choice === parts[2]);
    if (way) return { name: "add-project", way };
  }
  if (parts[0] === "projects" && parts.length >= 2 && parts.length <= 3) {
    let id: string;
    try { id = decodeURIComponent(parts[1]); } catch { return { name: "home" }; }
    if (!id || id.length > PROJECT_ID_MAX) return { name: "home" };
    if (parts[2] === "setup") return { name: "onboarding", id };
    return { name: "project", id, tab: pick(PROJECT_TABS, parts[2], "overview") };
  }
  return { name: "home" };
}

/** Writes a route as a location hash, the inverse of parseRoute.
 * Input: a route. Output: a hash string starting with "#/". */
export function formatRoute(route: Route): string {
  switch (route.name) {
    case "welcome": return "#/welcome";
    case "home": return "#/home";
    case "new-project": return `#/projects/new?step=${route.step}`;
    case "project": return `#/projects/${encodeURIComponent(route.id)}/${route.tab}`;
    case "add-project": return `#/projects/add/${route.way}`;
    case "onboarding": return `#/projects/${encodeURIComponent(route.id)}/setup`;
    case "settings": {
      const account = route.action && route.account ? `&account=${encodeURIComponent(route.account)}` : "";
      if (!route.action && route.tab === "plugins" && route.skill) return `#/settings/plugins?skill=${encodeURIComponent(route.skill)}`;
      return `#/settings/${route.tab}${route.action ? `?action=${route.action}${account}` : ""}`;
    }
  }
}

/** Moves the app to a route through the window location, so Back and Forward work.
 * Inputs: the route and whether to replace the current history entry. Output: none. */
export function navigate(route: Route, replace = false): void {
  const hash = formatRoute(route);
  if (replace) window.history.replaceState(null, "", hash);
  else window.location.hash = hash;
  if (replace) window.dispatchEvent(new HashChangeEvent("hashchange"));
}

/** Follows the window's hash route. Output: the current route. */
export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseRoute(typeof window === "undefined" ? "" : window.location.hash));
  useEffect(() => {
    const update = () => setRoute(parseRoute(window.location.hash));
    window.addEventListener("hashchange", update);
    update();
    return () => window.removeEventListener("hashchange", update);
  }, []);
  return route;
}
