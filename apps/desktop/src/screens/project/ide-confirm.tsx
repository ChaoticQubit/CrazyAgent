import { nextGeneration } from "../../api";
import { useEffect, useState } from "react";
import { loadIdeAsks, reviewMemory, type MemoryItem } from "../../api-projects";
import { ConfirmDialog } from "../../ui";
import { displayText } from "../conversation";
import { changeOutcome } from "./research";
import type { ProjectData } from "./shared";

export const IDE_ASK_POLL_MS = 2000;

/** The key of one IDE ask: the memory and the version the owner was asked about.
 * Input: the memory. Output: the key. */
export function askKey(item: Pick<MemoryItem, "id" | "version">): string {
  return `${item.id}@${item.version}`;
}

/** The desktop's confirm for a memory approval asked from the project's IDE. The IDE cannot
 * approve by itself: the owner sees "Approve this memory?" with the engine's own text here,
 * where the IDE cannot click, and Approve saves it through the owner review. Not now hides
 * that ask until the page is opened again. Input: the project data. Output: the dialog, or
 * nothing while no ask waits. */
export function IdeConfirm({ data }: { data: ProjectData }) {
  const [asks, setAsks] = useState<MemoryItem[]>([]);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState("");

  useEffect(() => {
    let live = true;
    const read = async () => {
      const result = await loadIdeAsks(data.id);
      if (live && result.kind === "ok") setAsks(result.value);
    };
    void read();
    const timer = window.setInterval(() => void read(), IDE_ASK_POLL_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [data.id]);

  const ask = asks.find((item) => !hidden.has(askKey(item)));
  if (ask === undefined) return null;

  function hide(item: MemoryItem) {
    setNote("");
    setHidden((old) => new Set(old).add(askKey(item)));
  }

  async function approve(item: MemoryItem) {
    setPending(true);
    setNote("");
    const result = await reviewMemory(data.id, item.id, item.version, { decision: "Keep" }, nextGeneration());
    setPending(false);
    const outcome = changeOutcome(result, "Saved to memory.", "approving memories");
    if (outcome.toast) data.notify(outcome.toast);
    if (outcome.note) {
      setNote(outcome.note);
      return;
    }
    hide(item);
  }

  return <ConfirmDialog open title="Approve this memory?" confirmLabel="Approve" cancelLabel="Not now" pending={pending}
    onConfirm={() => void approve(ask)} onCancel={() => hide(ask)}>
    <p>The IDE asked to save this memory for this project's agents.</p>
    <p className="pj-note-text">{displayText(ask.text)}</p>
    {note && <p role="alert">{note}</p>}
  </ConfirmDialog>;
}
