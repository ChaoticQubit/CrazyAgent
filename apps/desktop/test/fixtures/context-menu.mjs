import { blockWebContextMenu } from "../../src/ui/shell.tsx";

export const seen = [];

export function mount() {
  document.body.innerHTML = '<main><p id="plain">Plain text</p><input id="field"><textarea id="notes"></textarea>'
    + '<div contenteditable="true"><span id="editable">Editable</span></div><input id="tick" type="checkbox"></main>';
  blockWebContextMenu(document);
  window.addEventListener("contextmenu", (event) => seen.push({ id: event.target.id, prevented: event.defaultPrevented }));
}
