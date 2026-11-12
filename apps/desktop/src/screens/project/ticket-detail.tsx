import { useEffect, useId, useState } from "react";
import type { Failure, Result } from "../../api";
import {
  isClosed, kindLabel, linkView, loadHistory, loadTicket, moveTicket, newestFirst, notReady, openChangeSet, priorityLabel, statusLabel,
  trackerRefusal, triageBadge,
} from "../../api-tracker";
import type { TicketCheck, TicketCriterion, TicketDetail as Detail, TicketEvent, TicketStatus } from "../../api-tracker";
import { Button, Select } from "../../ui";
import { displayText } from "../conversation";
import { ago } from "./research";
import { Badge, NotReady } from "./shared";
import type { ProjectData } from "./shared";

const OPEN: readonly TicketStatus[] = ["triage", "backlog", "todo", "in_progress", "in_review"];
const CRITERION_STATES: Record<string, string> = { open: "Open", met: "Met", waived: "Waived" };
const CHECK_KINDS: Record<string, string> = { test: "Test", change: "Change", manual: "Manual", ci: "CI" };
const RESULTS: Record<string, string> = { pass: "Passed", fail: "Failed" };
const VERBS: Record<string, string> = {
  create: "created", edit: "edited", move: "moved", claim: "claimed", release: "released", depend: "added a blocker",
  undepend: "removed a blocker", criterion_add: "added a criterion", criterion_edit: "edited a criterion", check_add: "added a check",
  evidence_record: "recorded evidence", criterion_set: "set a criterion", link: "linked", unlink: "unlinked", close: "closed", reopen: "reopened",
};

function refusalOf(failure: Failure): string {
  return failure.kind === "error" ? trackerRefusal({ code: failure.code, cause: failure.cause }) : "";
}

function word(words: Record<string, string>, value: string): string {
  return Object.hasOwn(words, value) ? words[value] : displayText(value.replaceAll("_", " "));
}

/** One line of history: who did what, and the fields it changed. Input: the event. Output:
 * the line; agent text goes through `displayText`. */
export function eventLine(event: TicketEvent): string {
  const fields = event.fields.length > 0 ? ` (${event.fields.map((field) => displayText(field.replaceAll("_", " "))).join(", ")})` : "";
  return `${displayText(event.actor)} ${word(VERBS, event.op)}${fields}`;
}

function CheckLine({ check }: { check: TicketCheck }) {
  const latest = check.latest;
  return <li className="tk-check">
    <span className="pj-small pj-secondary">{word(CHECK_KINDS, check.kind)}</span>
    <code>{displayText(check.ref)}</code>
    {check.ref_elided && <span className="pj-small pj-secondary">(shortened)</span>}
    <span className="pj-small">{latest ? `${word(RESULTS, latest.result)}: ${displayText(latest.evidence)}` : "No evidence yet"}</span>
    {latest && <span className="pj-small pj-secondary">{`by ${displayText(latest.by)}`}</span>}
  </li>;
}

function CriterionItem({ criterion }: { criterion: TicketCriterion }) {
  return <li className="tk-criterion pj-stack-s">
    <div className="pj-row between">
      <span className="tk-criterion-text">{displayText(criterion.text)}</span>
      <Badge word={word(CRITERION_STATES, criterion.state)} tone={criterion.state === "open" ? "neutral" : "ok"} />
    </div>
    {criterion.checks.length > 0 && <ul className="tk-checks">{criterion.checks.map((check, at) => <CheckLine key={`${at}:${check.kind}:${check.ref}`} check={check} />)}</ul>}
  </li>;
}

/** One ticket: its fields, a status move, the body, the acceptance criteria with their checks
 * and latest evidence, live links with Open in IDE for a change set, and the history newest
 * first. Inputs: the ticket, the full history pages or null until read, now, whether an
 * action runs, the note to show and the handlers. Output: the view. */
export function TicketDetailView({ detail, history, now, busy, note, onMove, onOpenChangeSet, onHistory, onBack }: {
  detail: Detail;
  history: readonly (readonly TicketEvent[])[] | null;
  now: number;
  busy: boolean;
  note: string;
  onMove: (status: TicketStatus) => void;
  onOpenChangeSet: (changeSet: string) => void;
  onHistory: () => void;
  onBack: () => void;
}) {
  const { ticket } = detail;
  const field = useId();
  const choices = OPEN.filter((status) => status !== ticket.status);
  const [target, setTarget] = useState<TicketStatus>(choices[0]);
  const badge = triageBadge(ticket.status);
  const closed = isClosed(ticket.status);
  const events = newestFirst(detail.events, history);
  return <article className="pj-stack tk-detail" aria-label={`Ticket ${ticket.id}`}>
    <span><Button variant="quiet" icon="arrow-left" onClick={onBack}>Back to tickets</Button></span>
    <header className="pj-stack-s">
      <h3 className="tk-detail-title">{`${ticket.id} ${displayText(ticket.title)}`}</h3>
      <div className="pj-row">
        {badge ? <Badge word={badge.word} tone={badge.tone} /> : <Badge word={statusLabel(ticket.status)} tone={closed ? "ok" : "neutral"} />}
        <span className="pj-small pj-secondary">{kindLabel(ticket.kind)}</span>
        <span className="pj-small pj-secondary">{priorityLabel(ticket.priority)}</span>
        <span className="pj-small pj-secondary">{ticket.assignee ? `Assigned to ${displayText(ticket.assignee)}` : "Unassigned"}</span>
        <span className="pj-small pj-secondary">{`Reported by ${displayText(ticket.created_by)}, ${ago(ticket.created_ms, now)}`}</span>
      </div>
      {closed && <p className="pj-small pj-secondary">{`Closed by ${displayText(ticket.closed_by ?? "")}${ticket.close_reason ? `: ${displayText(ticket.close_reason)}` : "."}`}</p>}
    </header>
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
    {!closed && choices.length > 0 && <div className="pj-row tk-move">
      <Select id={field} label="Move to" value={target} onValue={(value) => setTarget(value as TicketStatus)}
        options={choices.map((status) => ({ value: status, label: statusLabel(status) }))} />
      <span><Button disabled={busy} pending={busy} pendingLabel="Moving..." onClick={() => onMove(target)}>Move</Button></span>
    </div>}
    <section className="pj-stack-s" aria-label="Description">
      {detail.body ? <p className="tk-body">{displayText(detail.body)}</p> : <p className="pj-small pj-secondary">No description.</p>}
      {detail.more && <p className="pj-small pj-secondary">Part of this ticket is too long to show here.</p>}
    </section>
    <section className="pj-stack-s" aria-label="Acceptance criteria">
      <h4>Acceptance criteria</h4>
      {detail.criteria.length === 0 ? <p className="pj-small pj-secondary">No criteria yet.</p>
        : <ol className="tk-criteria">{detail.criteria.map((criterion) => <CriterionItem key={criterion.position} criterion={criterion} />)}</ol>}
    </section>
    <section className="pj-stack-s" aria-label="Links">
      <h4>Links</h4>
      {detail.links.length === 0 ? <p className="pj-small pj-secondary">Nothing linked yet.</p> : <ul className="tk-links">
        {detail.links.map((link) => {
          const shown = linkView(link);
          const changeSet = shown.changeSet;
          return <li key={`${link.kind}:${link.target}`} className={`tk-link${shown.live ? "" : " gone"}`}>
            <span className="tk-link-title">{shown.title}</span>
            <span className="pj-small pj-secondary">{shown.state}</span>
            {changeSet !== null && <Button size="small" icon="file-code" disabled={busy} onClick={() => onOpenChangeSet(changeSet)}>Open in IDE</Button>}
          </li>;
        })}
      </ul>}
    </section>
    <section className="pj-stack-s" aria-label="History">
      <h4>History</h4>
      <ol className="tk-history">
        {events.map((event, at) => <li key={`${at}:${event.at_ms}`} className="pj-row between">
          <span>{eventLine(event)}</span>
          <span className="pj-small pj-secondary">{ago(event.at_ms, now)}</span>
        </li>)}
      </ol>
      {history === null && <span><Button variant="quiet" disabled={busy} onClick={onHistory}>Show full history</Button></span>}
    </section>
  </article>;
}

/** The detail view of one ticket, read from the engine: a move at the version read, which a
 * changed ticket refuses as stale and reads again, Open in IDE for a live change set, and
 * the full history on demand. Inputs: the project data, the ticket id and the back handler.
 * Output: the view. */
export function TicketDetail({ data, id, onBack }: { data: ProjectData; id: string; onBack: () => void }) {
  const [detail, setDetail] = useState<Result<Detail>>();
  const [history, setHistory] = useState<TicketEvent[][] | null>(null);
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  useEffect(() => {
    let current = true;
    void loadTicket(id).then((result) => { if (current) setDetail(result); });
    return () => { current = false; };
  }, [id, round]);

  async function move(status: TicketStatus) {
    if (detail?.kind !== "ok") return;
    setBusy(true);
    const result = await moveTicket(id, detail.value.ticket.version, status);
    setBusy(false);
    if (result.kind === "ok") {
      setNote("");
      data.notify(`${id} moved to ${statusLabel(status)}.`);
    } else setNote(refusalOf(result));
    setHistory(null);
    setRound((value) => value + 1);
  }

  async function reveal(changeSet: string) {
    setBusy(true);
    const result = await openChangeSet(data.id, changeSet);
    setBusy(false);
    if (result.kind === "ok") data.navigate(`/projects/${encodeURIComponent(data.id)}/ide`);
    else setNote(result.kind === "not_ready" ? "Open in IDE isn't in this engine yet." : refusalOf(result));
  }

  async function readHistory() {
    setBusy(true);
    const result = await loadHistory(id);
    setBusy(false);
    if (result.kind !== "ok") return setNote(refusalOf(result));
    setHistory(result.value.pages);
    if (result.value.more) setNote("This ticket's history is longer than CrazyAgent shows here.");
  }

  if (detail === undefined) return <p className="pj-muted pj-small" role="status">Loading the ticket...</p>;
  if (notReady(detail)) return <NotReady what="tickets aren't in this engine yet." />;
  if (detail.kind !== "ok") return <div className="pj-stack-s">
    <span><Button variant="quiet" icon="arrow-left" onClick={onBack}>Back to tickets</Button></span>
    <p className="pj-error" role="alert">{`Couldn't load ${id}. ${displayText(refusalOf(detail))}`}</p>
  </div>;
  return <TicketDetailView key={detail.value.ticket.version} detail={detail.value} history={history} now={data.now} busy={busy} note={note}
    onMove={(status) => void move(status)} onOpenChangeSet={(changeSet) => void reveal(changeSet)} onHistory={() => void readHistory()} onBack={onBack} />;
}
