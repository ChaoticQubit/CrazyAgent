import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { AccountChoiceView, ProjectAccountOrderView } from "../../src/screens/settings/account-choice.tsx";
import { MemoryView } from "../../src/screens/settings/memory.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const SCHEDULE = { strategy: "manual_rank", pools: [{ id: "pool-main", strategy: "balanced" }], projects: [{ id: "project-a", strategy: "use_it_or_lose_it" }], accounts: [], version: 3 };
const POOL = { id: "pool-main", name: "Main", summary: "1 Claude", members: [], project_count: 1 };
const nothing = () => undefined;
const MEMORY = { saved: { kind: "ok", value: { items: [], next: null, agent_writes: "save" } }, suggested: { kind: "ok", value: [] }, scope: "", projects: [], search: "", failure: "", busy: null,
  onSearch: nothing, onScope: nothing, onAdd: nothing, onSave: nothing, onSaveEdited: nothing, onDismiss: nothing, onDelete: nothing, onMore: nothing, onRetry: nothing, onImport: nothing };
let root = null;
let asked = 0;

/** Mounts one app settings view whose read never comes back after a save, so what stays on
 * screen is the pick. Inputs: "schedule", "project" or "memory", and whether a save is refused.
 * Output: nothing. */
export async function mount(view, refused) {
  root?.unmount();
  asked = 0;
  document.body.innerHTML = '<div id="mount"></div>';
  root = createRoot(document.getElementById("mount"));
  const answer = async () => { asked += 1; return refused ? { kind: "error", code: "refused", message: "Refused." } : { kind: "ok", value: undefined }; };
  const views = {
    schedule: [AccountChoiceView, { schedule: { kind: "ok", value: SCHEDULE }, pools: { kind: "ok", value: [POOL] }, names: new Map(), onChange: answer, onRetry: nothing }],
    project: [ProjectAccountOrderView, { schedule: { kind: "ok", value: SCHEDULE }, project: "project-a", pool: "pool-main", onChange: answer }],
    memory: [MemoryView, { ...MEMORY, onWrites: async () => { asked += 1; return refused ? "Refused." : ""; } }],
  };
  const [component, props] = views[view];
  await act(async () => { root.render(createElement(component, props)); });
}

/** Picks a value in one select as the owner does and lets its save run. Inputs: the select's id
 * and the value. Output: the value before, the value shown after and how many saves were asked. */
export async function pick(id, value) {
  const select = document.getElementById(id);
  const before = select.value;
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await new Promise((done) => setTimeout(done, 20));
  });
  return { before, shown: document.getElementById(id).value, asked };
}
