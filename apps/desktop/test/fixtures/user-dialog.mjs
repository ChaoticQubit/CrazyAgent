const words = (node) => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
/** Reads the user menu dialog holding the panel. Output: its title, whether the panel's own
 * heading is hidden under it, whether it fits the window and its body never scrolls sideways,
 * and how many dialogs are open; null when the panel is on the Settings page. */
export function dialogFrame() {
  const node = document.querySelector("dialog[open]:has(> .dialog-body > .user-dialog)");
  if (!node) return null;
  const box = node.getBoundingClientRect();
  const body = node.querySelector(".dialog-body");
  return {
    title: words(node.querySelector(".dialog-head h2")),
    headingHidden: getComputedStyle(node.querySelector(".settings-top h2")).display === "none",
    fits: box.left >= 0 && box.top >= 0 && box.right <= window.innerWidth && box.bottom <= window.innerHeight,
    sideways: body.scrollWidth > body.clientWidth,
    open: document.querySelectorAll("dialog[open]").length,
  };
}
