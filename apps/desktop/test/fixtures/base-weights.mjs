import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";

/**
 * Draws bare headings, bold words inside h1, h2, an h4 and a Markdown table header, and bold
 * text on a page and inside a project page, with the real stylesheets and no screen rule
 * around them. Input: none. Output: none.
 */
export function mount() {
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<h1 id="h1">Title <strong id="h1-strong">now</strong></h1><h2 id="h2"><b id="h2-b">Summary</b> of the run</h2><h3 id="h3">Group</h3>'
    + '<p><strong id="strong">Bold</strong> and <b id="b">bold</b></p>'
    + '<div class="pj"><h2 id="pj-h2"><strong id="pj-h2-strong">Summary</strong> of the run</h2><h3 id="pj-h3">Group</h3><p><strong id="pj-strong">Bold</strong></p>'
    + '<h4 id="pj-h4"><strong id="pj-h4-strong">Note</strong> on the run</h4>'
    + '<div class="markdown"><table><thead><tr><th id="md-th"><b id="md-th-b">Step</b> name</th></tr></thead></table></div></div>';
}

/** Reads each drawn element's computed weight. Output: an object of element id to weight. */
export function look() {
  return Object.fromEntries([...document.querySelectorAll("[id]")].map((node) => [node.id, getComputedStyle(node).fontWeight]));
}
