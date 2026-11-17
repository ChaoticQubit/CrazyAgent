import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import changes from "../../src/screens/changes/changes.css?inline";
import { Detail } from "../../src/screens/changes/index.tsx";

let root = null;
const none = () => {};

const SET = {
  id: "cs-1", agent: { kind: "sub", id: "cs-1", label: "Ada (@north) · Codex" }, state: "pending", base: "a".repeat(40), head: "b".repeat(40), files: 1, added: 4, removed: 1,
  last_outcome: "succeeded", checked: false, created_ms: 1, updated_ms: 1, restorable_until_ms: null, conflicts: [], regenerated: [], message: "", waiting: false, outside: false,
};

const DIFF = { files: [{ path: "src/cart.ts", old_path: null, status: "modified", kind: "text", marks: ["mode_change"] }], text: "+retry\n-once\n", next: null, truncated: false };

/**
 * Mounts the real change set detail of one pending set with the real stylesheets; its diff
 * reader answers at once with one marked file and a short diff. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, changes].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div class="pj" style="width: 900px; padding: 16px"><div id="root"></div></div>';
  root = createRoot(document.getElementById("root"));
  const load = async () => ({ kind: "ok", value: DIFF });
  flushSync(() => root.render(h(Detail, { set: SET, now: 2, busy: false, onAsk: none, onUpdate: none, onRestore: none, onMoved: none, load })));
}

/** Waits for the diff reader's answer to render. Output: a promise. */
export async function settled() {
  for (let round = 0; round < 20 && !document.querySelector(".ch-diff"); round += 1) {
    await new Promise((done) => requestAnimationFrame(done));
  }
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
 * Reads the detail's look and the tokens it should take. Output: the tokens, the title weight,
 * the Accept and Discard buttons, the file mark and the diff type.
 */
export function look() {
  const css = (selector) => getComputedStyle(document.querySelector(selector));
  const button = (word) => {
    const node = [...document.querySelectorAll(".ch-detail .btn")].find((item) => item.textContent === word);
    const style = getComputedStyle(node);
    return { background: style.backgroundColor, border: style.borderTopColor, color: style.color };
  };
  const diff = css(".ch-diff");
  return {
    tokens: Object.fromEntries(["primary", "on-primary", "danger", "text-2", "border"].map((name) => [name, token(name)])),
    title: css(".ch-detail h3").fontWeight,
    accept: button("Accept"),
    discard: button("Discard"),
    mark: { color: css(".ch-mark").color, border: css(".ch-mark").borderTopColor },
    diff: { size: diff.fontSize, line: diff.lineHeight, weight: diff.fontWeight, mono: diff.fontFamily === css(".pj-mono").fontFamily },
  };
}
