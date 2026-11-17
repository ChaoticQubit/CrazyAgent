import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";

/**
 * Draws bare headings and bold text on a page and inside a project page, with the real
 * stylesheets and no screen rule around them. Input: none. Output: none.
 */
export function mount() {
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<h1 id="h1">Title</h1><h2 id="h2">Section</h2><h3 id="h3">Group</h3>'
    + '<p><strong id="strong">Bold</strong> and <b id="b">bold</b></p>'
    + '<div class="pj"><h2 id="pj-h2">Section</h2><h3 id="pj-h3">Group</h3><p><strong id="pj-strong">Bold</strong></p></div>';
}

/** Reads each drawn element's computed weight. Output: an object of element id to weight. */
export function look() {
  return Object.fromEntries([...document.querySelectorAll("[id]")].map((node) => [node.id, getComputedStyle(node).fontWeight]));
}
