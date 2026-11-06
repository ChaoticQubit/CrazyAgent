import { nextGeneration } from "../../api";
import { useEffect, useState } from "react";
import { loadIdeAsks, reviewMemory, type IdeAsk } from "../../api-projects";
import { ConfirmDialog } from "../../ui";
import { displayText } from "../conversation";
import { changeOutcome } from "./research";
import type { ProjectData } from "./shared";

export const IDE_ASK_POLL_MS = 2000;

/** The key of one IDE ask: the memory, the version and the decision the owner was asked
 * about. Input: the ask. Output: the key. */
export function askKey(item: Pick<IdeAsk, "id" | "version" | "decision">): string {
  return `${item.id}@${item.version}:${item.decision}`;
}

const WORDS = {
  approve: { title: "Approve this memory?", confirm: "Approve", line: "The IDE asked to save this memory for this project's agents.", done: "Saved to memory.", missing: "approving memories" },
  reject: { title: "Reject this memory?", confirm: "Reject", line: "The IDE asked to reject this memory, so this project's agents will not use it.", done: "Memory rejected.", missing: "rejecting memories" },
} as const;

/** The desktop's confirm for a memory approval or rejection asked from the project's IDE.
 * The IDE cannot decide by itself: the owner sees the question with the engine's own text
 * here, where the IDE cannot click, and the confirm decides through the owner review. Not
 * now hides that ask until the page is opened again. Input: the project data. Output: the
 * dialog, or nothing while no ask waits. */
export function IdeConfirm({ data }: { data: ProjectData }) {
  const [asks, setAsks] = useState<IdeAsk[]>([]);
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

  function hide(item: IdeAsk) {
    setNote("");
    setHidden((old) => new Set(old).add(askKey(item)));
  }

  async function decide(item: IdeAsk) {
    setPending(true);
    setNote("");
    const choice = item.decision === "approve" ? { decision: "Keep" as const } : { decision: "Reject" as const };
    const result = await reviewMemory(data.id, item.id, item.version, choice, nextGeneration());
    setPending(false);
    const words = WORDS[item.decision];
    const outcome = changeOutcome(result, words.done, words.missing);
    if (outcome.toast) data.notify(outcome.toast);
    if (outcome.note) {
      setNote(outcome.note);
      return;
    }
    hide(item);
  }

  const words = WORDS[ask.decision];
  return <ConfirmDialog open title={words.title} confirmLabel={words.confirm} cancelLabel="Not now" pending={pending}
    onConfirm={() => void decide(ask)} onCancel={() => hide(ask)}>
    <p>{words.line}</p>
    <p className="pj-note-text">{displayText(ask.text)}</p>
    {note && <p role="alert">{note}</p>}
  </ConfirmDialog>;
}
