import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import consoleCss from "../../src/screens/project/console.css?inline";
import { Console } from "../../src/screens/project/console.tsx";
import { agentMentions } from "../../src/screens/project/console-model.ts";

let root = null;
const none = () => {};

const REPLY = [
  "# Plan",
  "",
  "## Steps",
  "",
  "See [the docs](https://example.com/docs) and run `make check`.",
  "",
  "| step | state |",
  "| --- | --- |",
  "| build | done |",
  "",
  "```",
  "make check",
  "```",
].join("\n");

const LINES = [
  { id: "1", kind: "owner", text: "Ask @onboarding-fix about the login" },
  { id: "2", kind: "reply", text: REPLY },
  { id: "3", kind: "delegate", text: "Ada (@onboarding-fix): Fix the login" },
  { id: "4", kind: "error", text: "The master stopped." },
];

const graphNode = (id, label) => ({ id, kind: "sub", parent: "master", label, harness: "codex", status: "working", status_label: null, account: null, terminal: null });

/**
 * Mounts the real master console with a tagged owner line, a Markdown reply with two headings,
 * a link, code, a table and a block, a hand-off line and an error line, and below it the
 * master console head and notes as live-master.tsx draws them, with the real stylesheets.
 * Input: the theme. Output: the composer's id.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, consoleCss].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div class="pj" style="width: 720px; padding: 16px"><div id="root" style="height: 560px"></div>'
    + '<div class="pj-master-live pj-console" id="master"><div class="pj-console-head"><span class="pj-console-title">Master</span>'
    + '<span class="pj-console-state ok">Running</span><span class="pj-console-detail">claude-work</span>'
    + '<span class="pj-console-actions"><button type="button" class="btn quiet">Settings</button></span></div>'
    + '<div class="pj-note warn" role="status"><p>Agents are paused.</p><p>Start them again to go on.</p></div>'
    + '<div class="pj-note" role="status"><p>The master stopped.</p></div></div></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(h(Console, {
    projectId: "p-1", lines: LINES, agents: agentMentions([graphNode("team-1", "Ada (@onboarding-fix)")]),
    onSend: none, onStop: none, onNavigate: none,
  })));
  return document.querySelector(".cs textarea").id;
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

const TOKENS = ["terminal-bg", "terminal-fg", "terminal-muted", "terminal-border", "terminal-selection", "terminal-surface", "terminal-bright-red", "terminal-bright-yellow", "terminal-yellow"];

/**
 * Reads the console's colors, type and the master head and notes. Output: the tokens and,
 * per part, the computed color, edge, fill, size or weight the restyle sets.
 */
export function look() {
  const css = (selector, pseudo) => getComputedStyle(document.querySelector(selector), pseudo);
  const notes = document.querySelectorAll("#master > .pj-note");
  return {
    tokens: Object.fromEntries(TOKENS.map((name) => [name, token(name)])),
    colors: {
      delegate: css(".cs-delegate").color,
      error: css(".cs-error").color,
      entry: css(".cs-entry").borderTopColor,
      tag: css(".cs-tag").backgroundColor,
      code: css(".cs .markdown p code").color,
      block: { fill: css(".cs .markdown pre").backgroundColor, edge: css(".cs .markdown pre").borderTopColor },
      cell: css(".cs .markdown td").borderTopColor,
      status: css(".cs-status").color,
    },
    type: {
      body: css(".cs").fontSize,
      status: css(".cs-status").fontSize,
      title: css(".cs .markdown h2").fontWeight,
      section: css(".cs .markdown h3").fontWeight,
      header: css(".cs .markdown th").fontWeight,
      link: css(".cs .markdown a").textDecorationLine,
    },
    head: { title: css("#master .pj-console-title").fontWeight, detail: css("#master .pj-console-detail").fontSize },
    notes: [...notes].map((note) => ({
      fill: getComputedStyle(note).backgroundColor,
      dot: getComputedStyle(note.querySelector("p"), "::before").content === "none" ? null : {
        color: getComputedStyle(note.querySelector("p"), "::before").backgroundColor,
        width: getComputedStyle(note.querySelector("p"), "::before").width,
      },
      second: note.querySelectorAll("p")[1] ? getComputedStyle(note.querySelectorAll("p")[1], "::before").content : null,
    })),
  };
}

/**
 * Reads the open @ menu's picked option once the composer holds "@". Output: the option count,
 * the picked option's fill, its ring color from the computed box shadow, and the terminal fill.
 */
export function picked() {
  const option = document.querySelector('.cs-menu [aria-selected="true"]');
  const style = option ? getComputedStyle(option) : null;
  return {
    options: document.querySelectorAll(".cs-menu [role=option]").length,
    fill: style?.backgroundColor ?? null,
    ring: style ? style.boxShadow.match(/rgba?\([^)]*\)/)?.[0] ?? null : null,
    inset: style ? style.boxShadow.endsWith("inset") : null,
    ground: getComputedStyle(document.querySelector(".cs-menu")).backgroundColor,
  };
}
