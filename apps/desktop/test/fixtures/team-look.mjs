import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import team from "../../src/screens/team/team-graph.css?inline";
import messages from "../../src/screens/team/agent-messages.css?inline";
import { layoutCards, layoutGraph } from "../../src/screens/team/graph.ts";
import { EdgeLegend, GraphCard, LinkBadges } from "../../src/screens/team/index.tsx";
import { AgentMessagesView } from "../../src/screens/team/agent-messages.tsx";
import { DEFAULT_MESSAGE_SETTINGS } from "../../src/api-messages.ts";

let root = null;
const none = () => {};

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, terminal: null, ...extra,
});

const GRAPH = {
  project_id: "chores",
  nodes: [
    node("m", "master", { label: "Master", status: "idle", terminal: "team-9", agent: 9 }),
    node("team-1", "lead", { parent: "m", label: "codex-1", status: "working", status_label: "Working", terminal: "team-1", agent: 1, branch: "crazyagent/agent-1" }),
    node("team-2", "sub", { parent: "team-1", label: "codex-2", status: "idle", terminal: "team-2", agent: 2, waiting: "question", presence: "live" }),
  ],
  edges: [{ from: "m", to: "team-1", task_id: 7, source: "master", text: "Task 7", state: "running", reply: null }],
};

const LINK = { from: "m", to: "team-1", path: "", badge: { x: 120, y: 20 }, label: "leads", live: false, tasks: 2, questions: 1, unread: 1, receipt: null, hookless: false };

const AT = 1_800_000_000_000;
const TIMELINE = {
  entries: [{ at_ms: AT, source: "event", id: 1, from: "agent", kind: "finished", text: "Finished the chart", task: 9, state: "delivered", result: "Built the chart", result_more: false }],
  older: false,
  settings: { ...DEFAULT_MESSAGE_SETTINGS, results: "full" },
};

/**
 * Mounts the real team graph parts with the real stylesheets: an open working lead card, a
 * card waiting on the owner with its chip, a link's label and count badges, the edge legend
 * with its live label, a live link and a message line, and one agent's Messages with a full
 * result. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, team, messages].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div style="width: 900px; padding: 16px"><div id="graph" style="position: relative; height: 420px"></div>'
    + '<svg class="tg-links" width="200" height="40"><path class="tg-link live" d="M0 0 L100 0" /><path class="tg-message" d="M0 10 L100 10" /></svg>'
    + '<div id="rest"></div></div>';
  const cards = layoutCards(layoutGraph(GRAPH));
  const placed = (id) => cards.find((item) => item.node.id === id);
  root = createRoot(document.getElementById("graph"));
  flushSync(() => root.render(h("div", null,
    h(GraphCard, { placed: placed("team-1"), graph: GRAPH, items: [], open: true, tipId: null, onOpen: none, onTip: none }),
    h(GraphCard, { placed: placed("team-2"), graph: GRAPH, items: [], open: false, tipId: null, onOpen: none, onTip: none }),
    h(LinkBadges, { link: LINK }),
    h(EdgeLegend),
    h(AgentMessagesView, { name: "Ada (@charts)", timeline: TIMELINE, problem: "", busy: false, onOlder: none, onOpenLink: none }),
  )));
}

/**
 * The color one token paints in the mounted theme, as the browser computes it.
 * Input: the token name without its dashes. Output: an rgb() string.
 */
function token(name) {
  const probe = document.createElement("span");
  probe.style.color = `var(--${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}

/**
 * Reads the type of one element. Input: the selector. Output: its size, line height, weight
 * and whether it is set in the mono family.
 */
function type(selector) {
  const style = getComputedStyle(document.querySelector(selector));
  const mono = getComputedStyle(document.documentElement).getPropertyValue("--mono").trim().split(",")[0];
  return { size: style.fontSize, line: style.lineHeight, weight: style.fontWeight, mono: style.fontFamily.startsWith(mono) };
}

/**
 * Reads the team graph's colors and type. Output: the tokens, the open card, the card lines,
 * the chip, the count badge, the plain and live link labels, the legend, the lines and the
 * Messages panel.
 */
export function look() {
  const css = (selector, pseudo) => getComputedStyle(document.querySelector(selector), pseudo);
  return {
    tokens: Object.fromEntries(["primary", "surface-3", "text"].map((name) => [name, token(name)])),
    open: { edge: css(".tg-card.open").borderTopColor, halo: css(".tg-card.open").boxShadow },
    card: { title: type(".tg-card.open .tg-card-title"), detail: type(".tg-card.open .tg-card-task").size, branch: type(".tg-card.open .tg-card-branch").size },
    chip: type(".tg-chip.warn"),
    badge: { edge: css(".tg-badge:not(.question)").borderTopColor, color: css(".tg-badge:not(.question)").color, ...type(".tg-badge:not(.question)") },
    label: type(".tg-legend .tg-edge-label:not(.live)"),
    live: {
      edge: css(".tg-legend .tg-edge-label.live").borderTopColor, fill: css(".tg-legend .tg-edge-label.live").backgroundColor,
      color: css(".tg-legend .tg-edge-label.live").color, weight: css(".tg-legend .tg-edge-label.live").fontWeight,
      dot: css(".tg-legend .tg-edge-label.live", "::before").backgroundColor,
    },
    legend: type(".tg-legend"),
    lines: { live: css(".tg-link.live").stroke, message: css(".tg-message").stroke },
    messages: {
      title: type(".am-title"), speaker: { weight: css(".am-speaker").fontWeight, color: css('.am-row[data-from="agent"] .am-speaker').color },
      time: css(".am-time").fontSize, state: css(".am-state").fontSize, from: css(".am-from").fontSize,
      bar: `${css(".am-md").borderLeftWidth} ${css(".am-md").borderLeftColor}`,
    },
  };
}
