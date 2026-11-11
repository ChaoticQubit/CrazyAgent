import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { Failure, Result } from "../../api";
import { acceptChange, changesRefusal, discardChange, loadChangeDiff, loadTargetBranches, notReady, restoreChange, updateChange } from "../../api-changes";
import type { ChangeDiff, ChangeSet, TargetBranches } from "../../api-changes";
import { Button, ConfirmDialog, Dialog, Select, TextArea } from "../../ui";
import { displayText } from "../conversation";
import { Badge, NotReady, ProjectEmpty } from "../project/shared";
import type { ProjectData } from "../project/shared";
import { acceptBlocked, acceptNote, acceptTarget, actions, agentName, branchLabel, changeSetName, checkedNote, commitMessage, conflictNote, diffText, fileLine, fileMarks, lastPageSeen, movedNote, movedOn, outcomeNote, regeneratedNote, restoreNote, reviewed, reviewOf, sizeLine, stateBadge, updatingNote } from "./model";
import type { Review } from "./model";
import { useChangeList } from "./poll";
import "./changes.css";

type Asking = { kind: "accept" | "discard"; set: ChangeSet } | null;

/** Shows diff text as plain text in a preformatted block: markup stays text and control
 * characters show as escapes. On the last page it calls `onEnd` once the end of the text
 * is on screen inside the block's own scroll, not when the page arrived (RVA-1). Inputs:
 * the diff text and, on the last page, what to do at its end. Output: the block. */
export function DiffText({ text, onEnd }: { text: string; onEnd?: () => void }) {
  const end = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const marker = end.current;
    if (!onEnd || marker === null) return;
    const watch = new IntersectionObserver((entries) => { if (entries.some((entry) => entry.isIntersecting)) onEnd(); });
    watch.observe(marker);
    return () => watch.disconnect();
  }, [onEnd, text]);
  return <pre className="ch-diff" aria-label="Diff">{diffText(text)}{onEnd && <span ref={end} className="ch-end" aria-hidden="true" />}</pre>;
}

/** The plain Not ready yet notice for an engine or build that cannot land changes yet. */
export function ChangesNotReady() {
  return <NotReady what="reviewing and landing agent changes isn't in this engine yet." />;
}

/** The Accept dialog: says where the changes land, lets the owner pick the local branch
 * they land on, the team's Land on branch first, and shows the proposed commit message for the
 * owner to edit first, hidden characters as escapes with a warning; Accept stays off while
 * the message is empty, too long or holds hidden characters.
 * Inputs: the change set, whether the accept runs, and the handlers; Accept gets the
 * message and the picked branch, null for the Land on branch. Output: the dialog. */
export function AcceptDialog({ set, pending, onCancel, onAccept }: {
  set: ChangeSet;
  pending: boolean;
  onCancel: () => void;
  onAccept: (message: string, target: string | null) => void;
}) {
  const shown = displayText(set.message);
  const [text, setText] = useState(shown);
  const [branches, setBranches] = useState<TargetBranches | null>(null);
  const [target, setTarget] = useState("");
  const id = useId();
  const branchId = useId();
  const message = commitMessage(text);
  useEffect(() => {
    let current = true;
    void loadTargetBranches(set.id).then((result) => {
      if (!current || result.kind !== "ok") return;
      setBranches(result.value);
      setTarget((result.value.land_on_problem ?? null) !== null ? "" : result.value.land_on ?? result.value.current ?? result.value.branches[0] ?? "");
    });
    return () => { current = false; };
  }, [set.id]);
  const { picked, text: lands } = acceptTarget(branches, target);
  const blocked = acceptBlocked(branches, target);
  return <Dialog open title="Accept these changes?" onClose={onCancel} busy={pending}
    footer={<><Button disabled={pending} onClick={onCancel}>Keep reviewing</Button>
      <Button variant="primary" pending={pending} disabled={message === null || blocked} onClick={() => { if (message !== null && !blocked) onAccept(message, picked); }}>Accept</Button></>}>
    <p>{lands}</p>
    {branches !== null && branches.branches.length > 0 &&
      <Select id={branchId} label="Branch" value={target} onValue={setTarget}
        options={[...(blocked ? [{ value: "", label: "Pick a branch" }] : []), ...branches.branches.map((name) => ({ value: name, label: branchLabel(branches, name) }))]} />}
    {shown !== set.message && <p role="alert">The proposed message held characters you cannot see. They are shown as escapes: check it before you accept.</p>}
    <TextArea id={id} label="Commit message" value={text} onValue={setText} rows={6}
      error={message === null ? "Write a commit message of up to 4096 bytes, with no hidden characters, to accept." : undefined} />
  </Dialog>;
}

function refusalOf(failure: Failure): string {
  return failure.kind === "error" ? changesRefusal({ code: failure.code, cause: failure.cause }) : "";
}

/** One change set's details, its diff a page at a time and the owner's actions on it.
 * Inputs: the change set, now, whether an action runs, the action handlers, what a page
 * refused as moved on does, and how a diff page is read, the engine's by default.
 * Output: the section; a diff page shows only under the change set, head and page it was
 * read at, so an old head's end marker never counts for a new one. */
export function Detail({ set, now, busy, onAsk, onUpdate, onRestore, onMoved, load = loadChangeDiff }: {
  set: ChangeSet;
  now: number;
  busy: boolean;
  onAsk: (asking: Asking) => void;
  onUpdate: (set: ChangeSet) => void;
  onRestore: (set: ChangeSet) => void;
  onMoved: () => void;
  load?: typeof loadChangeDiff;
}) {
  const [pages, setPages] = useState<number[]>([0]);
  const page = pages[pages.length - 1];
  const [read, setRead] = useState<{ id: string; head: string; page: number; result: Result<ChangeDiff> }>();
  const [review, setReview] = useState<Review>(() => reviewOf(null, set));
  const [staleAt, setStaleAt] = useState<ChangeSet | null>(null);
  const carried = reviewOf(review, set);
  if (carried !== review) {
    setReview(carried);
    setPages([0]);
  }
  if (staleAt !== null && staleAt !== set) {
    setStaleAt(null);
    setPages([0]);
  }
  const allowed = actions(set, now);
  const settled = reviewed(carried);
  const notes = [movedNote(carried), conflictNote(set), regeneratedNote(set), checkedNote(set), outcomeNote(set), updatingNote(set), restoreNote(set, now)].filter((note): note is string => note !== null);
  const head = carried.head;
  const diff = read?.id === set.id && read.head === head && read.page === page ? read.result : undefined;
  useEffect(() => {
    if (staleAt !== null) return;
    let current = true;
    setRead(undefined);
    void load(set.id, head, page).then((result) => {
      if (!current) return;
      if (result.kind === "error" && result.code === "changes_stale") {
        setStaleAt(set);
        setReview(movedOn);
        onMoved();
      } else setRead({ id: set.id, head, page, result });
    });
    return () => { current = false; };
  }, [set.id, head, page, load, staleAt]);
  const value = diff?.kind === "ok" ? diff.value : null;
  const endSeen = useCallback(() => setReview((shown) => lastPageSeen(shown, head)), [head]);
  const asked = { ...set, head };
  return <section className="pj-stack-s ch-detail" aria-label={`Changes from ${agentName(set)}`}>
    <div className="pj-row between">
      <h3>{changeSetName(set)}</h3>
      <Badge word={stateBadge(set).word} tone={stateBadge(set).tone} />
    </div>
    <p className="pj-secondary pj-small">{sizeLine(set)}</p>
    {notes.map((note) => <p className="pj-note warn" key={note}>{note}</p>)}
    <div className="pj-row">
      {allowed.accept && <button type="button" className="btn primary" disabled={busy || !settled} onClick={() => onAsk({ kind: "accept", set: asked })}>Accept</button>}
      {allowed.update && <button type="button" className="btn" disabled={busy} onClick={() => onUpdate(set)}>Ask agent to update</button>}
      {allowed.discard && <button type="button" className="btn danger-text" disabled={busy || !settled} onClick={() => onAsk({ kind: "discard", set: asked })}>Discard</button>}
      {allowed.restore && <button type="button" className="btn" disabled={busy} onClick={() => onRestore(set)}>Restore</button>}
    </div>
    {diff === undefined && <p className="pj-muted pj-small" role="status">Loading changes...</p>}
    {diff !== undefined && notReady(diff) && <ChangesNotReady />}
    {diff?.kind === "error" && !notReady(diff) && <p className="pj-error" role="alert">Couldn't load the changes. {refusalOf(diff)}</p>}
    {value && <>
      <ul className="ch-files" aria-label="Changed files">{value.files.map((file) => <li key={`${file.old_path ?? ""}>${file.path}`}>
        <span className="pj-mono">{fileLine(file)}</span>
        {fileMarks(file).map((mark) => <span className="ch-mark pj-small" key={mark}>{mark}</span>)}
      </li>)}</ul>
      <DiffText text={value.text} onEnd={value.next === null ? endSeen : undefined} />
      {value.truncated && <p className="pj-note warn">Part of this diff is not shown: it is longer than CrazyAgent shows at once.</p>}
      {(pages.length > 1 || value.next !== null) && <div className="pj-row">
        <button type="button" className="btn" disabled={pages.length === 1} onClick={() => setPages(pages.slice(0, -1))}>Previous page</button>
        <span className="pj-small pj-secondary">Page {pages.length}</span>
        <button type="button" className="btn" disabled={value.next === null} onClick={() => { if (value.next !== null) setPages([...pages, value.next]); }}>Next page</button>
      </div>}
    </>}
  </section>;
}

/** One change set in the Changes list: its agent, its state badge and its size. Inputs: the
 * change set, whether it is the one shown and what selecting it does. Output: the row button. */
export function ChangeRow({ set, active, onSelect }: { set: ChangeSet; active: boolean; onSelect: (id: string) => void }) {
  const badge = stateBadge(set);
  return <button type="button" className={`ch-row${active ? " active" : ""}`} aria-current={active} onClick={() => onSelect(set.id)}>
    <strong>{changeSetName(set)}</strong>
    <Badge word={badge.word} tone={badge.tone} />
    <span className="pj-small pj-secondary">{sizeLine(set)}</span>
  </button>;
}

/** Changes tab: every change set the project's agents made, newest first, with its diff and
 * the owner's Accept, Discard, Restore and Ask agent to update. A refused action is shown only
 * while the set it was about is the one shown, never under another set. Input: the project
 * data. Output: the tab; the list rereads a few seconds after each reply while it is open. */
export function ChangesTab({ data }: { data: ProjectData }) {
  const [list, reread] = useChangeList(data.id);
  const [selected, setSelected] = useState<string | null>(null);
  const [asking, setAsking] = useState<Asking>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ set: string; text: string } | null>(null);
  const sets = list?.kind === "ok" ? list.value : [];
  const shown = sets.find((set) => set.id === selected) ?? sets[0] ?? null;

  async function settle<T>(set: ChangeSet, result: Result<T>, done: (value: T) => string | null) {
    setBusy(false);
    setAsking(null);
    if (result.kind === "ok") {
      setNote(null);
      const message = done(result.value);
      if (message !== null) data.notify(message);
    } else setNote({ set: set.id, text: notReady(result) ? "Landing changes isn't ready in this engine yet." : refusalOf(result) });
    reread();
  }

  async function accept(set: ChangeSet, message: string, target: string | null) {
    setBusy(true);
    await settle(set, await acceptChange(set.id, set.head, message, target), acceptNote);
  }

  async function discard(set: ChangeSet) {
    setBusy(true);
    await settle(set, await discardChange(set.id, set.head), () => "Discarded. You can restore these changes for 7 days.");
  }

  async function update(set: ChangeSet) {
    setBusy(true);
    await settle(set, await updateChange(set.id), () => "Your branch is merged into the agent's copy. The list shows when the agent picks it up.");
  }

  async function restore(set: ChangeSet) {
    setBusy(true);
    await settle(set, await restoreChange(set.id), () => "Restored. The changes are ready to review again.");
  }

  if (list === undefined) return <p className="pj-muted pj-small" role="status">Loading changes...</p>;
  if (notReady(list)) return <ChangesNotReady />;
  return <div className="pj-stack">
    {list.kind === "error" && <p className="pj-error" role="alert">Couldn't load changes. {refusalOf(list)}</p>}
    {note !== null && note.set === shown?.id && <p className="pj-note warn" role="alert">{note.text}</p>}
    {list.kind === "ok" && sets.length === 0 && <ProjectEmpty title="No changes yet">
      When the master or a sub-agent edits files, its changes wait here for you to review and accept.
    </ProjectEmpty>}
    {sets.length > 0 && <div className="ch-layout">
      <ul className="pj-rows ch-list" aria-label="Change sets">{sets.map((set) => <li key={set.id}>
        <ChangeRow set={set} active={shown?.id === set.id} onSelect={setSelected} />
      </li>)}</ul>
      {shown && <Detail set={shown} now={data.now} busy={busy} onAsk={setAsking} onUpdate={(set) => void update(set)} onRestore={(set) => void restore(set)} onMoved={reread} />}
    </div>}
    {asking?.kind === "accept" && <AcceptDialog key={`${asking.set.id}:${asking.set.head}`} set={asking.set} pending={busy}
      onCancel={() => setAsking(null)} onAccept={(message, target) => void accept(asking.set, message, target)} />}
    <ConfirmDialog open={asking?.kind === "discard"} pending={busy} title="Discard these changes?" confirmLabel="Discard" cancelLabel="Keep reviewing"
      onCancel={() => setAsking(null)} onConfirm={() => { if (asking) void discard(asking.set); }}>
      The agent's working copy goes back to your branch. You can restore these changes for 7 days.
    </ConfirmDialog>
  </div>;
}
