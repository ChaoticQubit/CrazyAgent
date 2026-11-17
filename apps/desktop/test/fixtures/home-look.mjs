import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { HomeView } from "../../src/screens/dashboard/index.tsx";

let root = null;

const ok = (value) => ({ kind: "ok", value });
const tool = (name, state) => ({ name, label: name, state });
const found = { state: "found", version: "3.5a" };

/**
 * Mounts the real Home with the real stylesheet: one Needs you row naming a project, one
 * project card with a research line and a working-agents line, and the Get set up panel, since
 * no account is signed in and no pool exists. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = shell;
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root" style="width: 1100px; padding: 24px"></div>';
  root = createRoot(document.getElementById("root"));
  const harness = { id: "codex", name: "codex", status: { kind: "ready" }, status_label: "", executable: null, headless: "", tools: { kind: "switchable" },
    model_selection: true, account_kinds: [], aliases: [] };
  flushSync(() => root.render(h(HomeView, {
    projects: ok([{ id: "p1", name: "Harbor", repository_count: 0 }]),
    accounts: ok([]), pools: ok([]), harnesses: ok([harness]),
    tools: ok({ dependencies: [tool("tmux", found), tool("uv", found), tool("graphify", found)], pack: [], running: false }),
    activity: new Map([["p1", { research: "Research running", agents: "2 agents working", waiting: false, live: true }]]),
    alerts: new Map([["p1", 2]]),
    onRetryProjects: () => {},
  })));
}

/**
 * Reads the Home parts the restyle changes. Output: the card name, line and meta type, the
 * Needs you project name and box, and the Get set up item title weight.
 */
export function look() {
  const css = (selector) => getComputedStyle(document.querySelector(selector));
  const type = (selector) => ({ size: css(selector).fontSize, line: css(selector).lineHeight, weight: css(selector).fontWeight });
  return {
    cards: document.querySelectorAll(".project-card").length,
    name: type(".project-card .title-row h3"),
    activity: type(".project-card .activity"),
    meta: type(".project-card .meta"),
    need: { project: css(".need strong").fontWeight, count: document.querySelectorAll(".need").length },
    setup: { items: document.querySelectorAll(".setup-panel li").length, title: css(".setup-panel li strong").fontWeight },
  };
}
