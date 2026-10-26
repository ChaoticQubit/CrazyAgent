import { useEffect, useState } from "react";
import type { Failure, Result } from "../../api";
import { acceptChange, changesRefusal, discardChange, loadChangeDiff, loadChanges, notReady, restoreChange, updateChange } from "../../api-changes";
import type { ChangeDiff, ChangeSet } from "../../api-changes";
import { ConfirmDialog } from "../../ui";
import { Badge, NotReady, ProjectEmpty } from "../project/shared";
import type { ProjectData } from "../project/shared";
import { actions, agentName, checkedNote, diffText, fileLine, fileMarks, outcomeNote, restoreNote, sizeLine, stateBadge, updatingNote } from "./model";
import "./changes.css";

const LIST_POLL_MS = 5000;

type Asking = { kind: "accept" | "discard"; set: ChangeSet } | null;

/** Shows diff text as plain text in a preformatted block: markup stays text and control
 * characters show as escapes. Input: the diff text. Output: the block. */
export function DiffText({ text }: { text: string }) {
  return <pre className="ch-diff" aria-label="Diff">{diffText(text)}</pre>;
}

/** The plain Not ready yet notice for an engine or build that cannot land changes yet. */
export function ChangesNotReady() {
  return <NotReady what="reviewing and landing agent changes isn't in this engine yet." />;
}

function refusalOf(failure: Failure): string {
  return failure.kind === "error" ? changesRefusal({ code: failure.code, cause: failure.cause }) : "";
}

function Detail({ set, now, busy, onAsk, onUpdate, onRestore }: {
  set: ChangeSet;
  now: number;
  busy: boolean;
  onAsk: (asking: Asking) => void;
  onUpdate: (set: ChangeSet) => void;
  onRestore: (set: ChangeSet) => void;
}) {
  const [page, setPage] = useState(0);
  const [diff, setDiff] = useState<Result<ChangeDiff>>();
  const allowed = actions(set, now);
  const notes = [checkedNote(set), outcomeNote(set), updatingNote(set), restoreNote(set, now)].filter((note): note is string => note !== null);
  useEffect(() => { setPage(0); }, [set.id]);
  useEffect(() => {
    let current = true;
    setDiff(undefined);
    void loadChangeDiff(set.id, page).then((result) => { if (current) setDiff(result); });
    return () => { current = false; };
  }, [set.id, set.head, page]);
  const value = diff?.kind === "ok" ? diff.value : null;
  return <section className="pj-stack-s ch-detail" aria-label={`Changes from ${agentName(set)}`}>
    <div className="pj-row between">
      <h3>{agentName(set)}</h3>
      <Badge word={stateBadge(set).word} tone={stateBadge(set).tone} />
    </div>
    <p className="pj-secondary pj-small">{sizeLine(set)}</p>
    {notes.map((note) => <p className="pj-note warn" key={note}>{note}</p>)}
    <div className="pj-row">
      {allowed.accept && <button type="button" className="btn primary" disabled={busy} onClick={() => onAsk({ kind: "accept", set })}>Accept</button>}
      {allowed.update && <button type="button" className="btn" disabled={busy} onClick={() => onUpdate(set)}>Ask agent to update</button>}
      {allowed.discard && <button type="button" className="btn danger-text" disabled={busy} onClick={() => onAsk({ kind: "discard", set })}>Discard</button>}
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
      <DiffText text={value.text} />
      {value.truncated && <p className="pj-note warn">Part of this diff is not shown: it is longer than CrazyAgent shows at once.</p>}
      {value.pages > 1 && <div className="pj-row">
        <button type="button" className="btn" disabled={page === 0} onClick={() => setPage(page - 1)}>Previous page</button>
        <span className="pj-small pj-secondary">Page {page + 1} of {value.pages}</span>
        <button type="button" className="btn" disabled={page + 1 >= value.pages} onClick={() => setPage(page + 1)}>Next page</button>
      </div>}
    </>}
  </section>;
}

/** Changes tab: every change set the project's agents made, newest first, with its diff and
 * the owner's Accept, Discard, Restore and Ask agent to update. Input: the project data.
 * Output: the tab; the list rereads every few seconds while it is open. */
export function ChangesTab({ data }: { data: ProjectData }) {
  const [list, setList] = useState<Result<ChangeSet[]>>();
  const [round, setRound] = useState(0);
  const [selected, setSelected] = useState<string | null>(null);
  const [asking, setAsking] = useState<Asking>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  useEffect(() => {
    let current = true;
    void loadChanges(data.id).then((result) => { if (current) setList(result); });
    const timer = setInterval(() => setRound((value) => value + 1), LIST_POLL_MS);
    return () => { current = false; clearInterval(timer); };
  }, [data.id, round]);
  const sets = list?.kind === "ok" ? list.value : [];
  const shown = sets.find((set) => set.id === selected) ?? sets[0] ?? null;

  async function settle<T>(result: Result<T>, done: (value: T) => string) {
    setBusy(false);
    setAsking(null);
    if (result.kind === "ok") {
      setNote("");
      data.notify(done(result.value));
    } else setNote(notReady(result) ? "Landing changes isn't ready in this engine yet." : refusalOf(result));
    setRound((value) => value + 1);
  }

  async function confirm() {
    if (!asking) return;
    const { kind, set } = asking;
    setBusy(true);
    if (kind === "accept") await settle(await acceptChange(set.id, set.head), (value) => `Accepted as ${value.commit.slice(0, 7)} on ${value.branch}.`);
    else await settle(await discardChange(set.id, set.head), () => "Discarded. You can restore these changes for 7 days.");
  }

  async function update(set: ChangeSet) {
    setBusy(true);
    await settle(await updateChange(set.id), () => "The agent is updating its changes onto your branch.");
  }

  async function restore(set: ChangeSet) {
    setBusy(true);
    await settle(await restoreChange(set.id), () => "Restored. The changes are ready to review again.");
  }

  if (list === undefined) return <p className="pj-muted pj-small" role="status">Loading changes...</p>;
  if (notReady(list)) return <ChangesNotReady />;
  return <div className="pj-stack">
    {list.kind === "error" && <p className="pj-error" role="alert">Couldn't load changes. {refusalOf(list)}</p>}
    {note && <p className="pj-note warn" role="alert">{note}</p>}
    {list.kind === "ok" && sets.length === 0 && <ProjectEmpty title="No changes yet">
      When the master or a sub-agent edits files, its changes wait here for you to review and accept.
    </ProjectEmpty>}
    {sets.length > 0 && <div className="ch-layout">
      <ul className="pj-rows ch-list" aria-label="Change sets">{sets.map((set) => <li key={set.id}>
        <button type="button" className={`ch-row${shown?.id === set.id ? " active" : ""}`} aria-current={shown?.id === set.id} onClick={() => setSelected(set.id)}>
          <strong>{agentName(set)}</strong>
          <Badge word={stateBadge(set).word} tone={stateBadge(set).tone} />
          <span className="pj-small pj-secondary">{sizeLine(set)}</span>
        </button>
      </li>)}</ul>
      {shown && <Detail set={shown} now={data.now} busy={busy} onAsk={setAsking} onUpdate={(set) => void update(set)} onRestore={(set) => void restore(set)} />}
    </div>}
    <ConfirmDialog open={asking !== null} pending={busy} title={asking?.kind === "accept" ? "Accept these changes?" : "Discard these changes?"}
      confirmLabel={asking?.kind === "accept" ? "Accept" : "Discard"} cancelLabel="Keep reviewing"
      onCancel={() => setAsking(null)} onConfirm={() => void confirm()}>
      {asking?.kind === "accept"
        ? "They land as one commit on the branch checked out in your project folder, under your own Git name. CrazyAgent did not check them."
        : "The agent's working copy goes back to your branch. You can restore these changes for 7 days."}
    </ConfirmDialog>
  </div>;
}
