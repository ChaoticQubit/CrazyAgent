import { useEffect, useState } from "react";
import { nextGeneration } from "../../api";
import { markTeamSeen, setWakeRule } from "../../api-projects";
import type { TeamNotification, TeamStatus, WakeRule, WakeRuleLevel, WakeRuleTarget } from "../../engine-client";
import { Button, Notice, Select, StatusBadge, Switch, TextField } from "../../ui";
import { displayText } from "../conversation";
import { changeNote } from "./controls";
import { WAKE_KINDS, capInput, eventLine, eventState, ruleSource, withKind } from "./wake";

const RECENT_SHOWN = 10;

function targetKey(target: WakeRuleTarget): string {
  if (target.level === "app") return "app";
  return target.level === "project" ? `project-${target.project_id}` : `agent-${target.project_id}-${target.agent}`;
}

/** Edits the wake rule of one level: which sub-agent events wake the master and how many wakes
 * an hour it may take before auto-wake pauses. A switch or a new limit saves this level's own
 * rule; Reset clears it so the level above applies. Inputs: the level, its own rule or null,
 * the rule in effect, the level it falls back to and a reload. Output: the editor. */
export function WakeRuleEditor({ target, own, effective, fallback, reload }: {
  target: WakeRuleTarget;
  own: WakeRule | null;
  effective: WakeRule;
  fallback: WakeRuleLevel;
  reload: () => void;
}) {
  const shown = own ?? effective;
  const key = targetKey(target);
  const [cap, setCap] = useState(String(shown.hourly_cap));
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  useEffect(() => setCap(String(shown.hourly_cap)), [key, shown.hourly_cap]);
  const checked = capInput(cap);

  async function save(rule: WakeRule | null) {
    if (busy) return;
    setBusy(true);
    const message = changeNote(await setWakeRule(target, rule, nextGeneration()), "wake rules");
    setBusy(false);
    setNote(message);
    reload();
  }

  return <div className="tg-wake" role="group" aria-labelledby={`tg-wake-${key}-title`}>
    <p className="tg-terminal-note" id={`tg-wake-${key}-title`}>{ruleSource(own !== null, fallback)}</p>
    {WAKE_KINDS.map(({ kind, label }) => <Switch key={kind} id={`tg-wake-${key}-${kind}`} label={label} checked={shown[kind]}
      onChecked={(on) => void save(withKind(shown, kind, on))} disabledReason={busy ? "Saving the last change." : undefined} />)}
    <form className="pj-row" onSubmit={(event) => {
      event.preventDefault();
      if (checked.kind === "ok") void save({ ...shown, hourly_cap: checked.cap });
    }}>
      <TextField id={`tg-wake-${key}-cap`} label="Wakes an hour before auto-wake pauses" inputMode="numeric" value={cap} onValue={setCap}
        maxLength={3} error={checked.kind === "invalid" ? checked.reason : undefined} helper="0 never wakes the master." />
      <Button type="submit" size="small" disabled={busy || checked.kind !== "ok" || checked.cap === shown.hourly_cap}>Save limit</Button>
      {own && <Button size="small" variant="quiet" disabled={busy} onClick={() => void save(null)}>Reset</Button>}
    </form>
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
  </div>;
}

/** The owner's notifications of one project with a Mark as seen action for the unseen ones.
 * Inputs: the project id, its notifications, newest first, and a reload. Output: the list. */
export function NotificationList({ projectId, notifications, reload }: {
  projectId: string;
  notifications: TeamNotification[];
  reload: () => void;
}) {
  const [note, setNote] = useState("");
  const unseen = notifications.filter((item) => item.seen_ms === null);
  if (notifications.length === 0) return null;
  async function seen() {
    const upto = Math.max(...unseen.map((item) => item.id));
    setNote(changeNote(await markTeamSeen(projectId, upto, nextGeneration()), "notifications"));
    reload();
  }
  return <section aria-label="Notifications">
    <div className="pj-row between">
      <h4>Notifications</h4>
      {unseen.length > 0 && <Button size="small" onClick={() => void seen()}>Mark {unseen.length} as seen</Button>}
    </div>
    <ul className="pj-rows">{notifications.map((item) => <li key={item.id} className="pj-row">
      <StatusBadge word={item.seen_ms === null ? "New" : "Seen"} tone={item.seen_ms === null ? "info" : "neutral"} />
      <span>{displayText(item.text)}</span>
    </li>)}</ul>
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
  </section>;
}

/** What the team's sub-agents told the master and how they wake it, on the Agents tab: the
 * paused auto-wake notice, agents waiting for an answer, the owner's notifications, the latest
 * events and the project and agent wake rules. Inputs: the project id, the team status and a
 * reload. Output: the panel. */
export function TeamEventsPanel({ projectId, status, reload }: { projectId: string; status: TeamStatus; reload: () => void }) {
  const { events } = status;
  const subs = status.agents.filter((agent) => agent.role === "sub");
  const [agent, setAgent] = useState("");
  const chosen = subs.find((item) => String(item.id) === agent);
  const label = (id: number) => {
    const found = status.agents.find((item) => item.id === id);
    return found ? found.label : `Agent ${id}`;
  };
  const own = chosen ? events.wake.agents.find((item) => item.agent === chosen.id)?.rule ?? null : null;
  const above: WakeRuleLevel = events.wake.project ? "project" : events.wake.app ? "app" : "shipped";
  return <section className="tg-events" aria-label="Sub-agent events">
    <h3>Sub-agent events</h3>
    {events.paused && <Notice tone="warn" alert>Auto-wake of the master is paused: the hourly limit was reached. Events wait in the queue.</Notice>}
    {events.waiting.length > 0 && <ul className="pj-rows" aria-label="Waiting for an answer">{events.waiting.map((item) => <li key={item.agent} className="pj-row">
      <StatusBadge word="Waiting" tone="warn" /><strong>{displayText(label(item.agent))}</strong><span>{displayText(item.text)}</span>
    </li>)}</ul>}
    <NotificationList projectId={projectId} notifications={events.notifications} reload={reload} />
    {events.recent.length > 0 ? <ul className="pj-rows" aria-label="Latest events">{events.recent.slice(0, RECENT_SHOWN).map((item) => <li key={item.id} className="pj-row">
      <span className="grow">{displayText(eventLine(item))}</span><span className="pj-secondary">{eventState(item.state)}</span>
    </li>)}</ul> : <p className="tg-terminal-note">No sub-agent events yet.</p>}
    <h4>Wake the master for this project</h4>
    <WakeRuleEditor target={{ level: "project", project_id: projectId }} own={events.wake.project}
      effective={events.wake.effective} fallback={events.wake.app ? "app" : "shipped"} reload={reload} />
    {subs.length > 0 && <>
      <h4>Wake rule of one agent</h4>
      <Select id={`tg-wake-agent-${projectId}`} label="Agent" value={agent} onValue={setAgent}
        options={[{ value: "", label: "Pick an agent" }, ...subs.map((item) => ({ value: String(item.id), label: label(item.id) }))]} />
      {chosen && <WakeRuleEditor target={{ level: "agent", project_id: projectId, agent: chosen.id }} own={own}
        effective={own ?? events.wake.effective} fallback={above} reload={reload} />}
    </>}
  </section>;
}
