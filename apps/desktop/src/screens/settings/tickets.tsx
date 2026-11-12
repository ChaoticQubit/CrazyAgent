import { useState } from "react";
import type { Result } from "../../api";
import {
  NOT_ATTACHED_LINE, TICKET_EFFECT_LINE, TICKET_SETTINGS_STALE_LINE, belowLabel, canUseAppDefault, checkedPage, draftOf, draftValue,
  draftsChanged, keyText, loadTicketSettings, optionText, saveTicketSettings, ticketSettingsRefusal, valuesOf,
} from "../../api-ticket-settings";
import type { KeyDraft, TicketKey, TicketScope, TicketSettingRow, TicketSettingsPage } from "../../api-ticket-settings";
import { useLoad, type Loaded } from "../../state";
import { Button, LoadError, Notice, NotReady, Panel, RadioCard, Skeleton, TextField } from "../../ui";
import "./tickets.css";

/** What one setting's choices need: the row, the page's scope, its draft and the change
 * handler. */
export interface TicketKeyFieldProps {
  row: TicketSettingRow;
  scope: TicketScope;
  draft: KeyDraft;
  onDraft: (draft: KeyDraft) => void;
}

/** One ticket setting: its label and help, then the value below, every option and, for the
 * cycle length, a custom number of days, each with one line saying what it does. Input: the
 * field props. Output: the fieldset. */
export function TicketKeyField({ row, scope, draft, onDraft }: TicketKeyFieldProps) {
  const name = `ts-${row.key}`;
  const text = keyText(row.key);
  const checked = draftValue(row, draft);
  return <fieldset className="stack ts-key" aria-describedby={`${name}-help`}>
    <legend className="ts-label">{text.label}</legend>
    <p id={`${name}-help`} className="helper">{text.help}</p>
    <RadioCard name={name} value="below" checked={draft.kind === "below"} onChecked={() => onDraft({ kind: "below" })}
      title={belowLabel(row, scope)} details={[optionText(row.key, row.below).line]} />
    {row.options.map((option) => {
      const words = optionText(row.key, option);
      return <RadioCard key={String(option)} name={name} value={String(option)} checked={draft.kind === "option" && draft.value === option}
        onChecked={() => onDraft({ kind: "option", value: option })} title={words.label} details={[words.line]} />;
    })}
    {row.custom && <RadioCard name={name} value="custom" checked={draft.kind === "custom"}
      onChecked={() => onDraft({ kind: "custom", text: draft.kind === "custom" ? draft.text : "" })}
      title="Custom" details={[`Pick any length from ${row.custom.min} to ${row.custom.max} days.`]} />}
    {draft.kind === "custom" && <TextField id={`${name}-custom`} label="Days" inputMode="numeric" value={draft.text}
      onValue={(text) => onDraft({ kind: "custom", text })} error={"problem" in checked ? checked.problem : undefined} />}
  </fieldset>;
}

/** Everything a ticket settings page shows and the actions it offers, without engine calls. */
export interface TicketSettingsViewProps {
  page: Loaded<TicketSettingsPage>;
  scope: TicketScope;
  projectId: string | null;
  drafts: Partial<Record<TicketKey, KeyDraft>>;
  busy: boolean;
  note: string;
  onDraft: (key: TicketKey, draft: KeyDraft) => void;
  onSave: () => void;
  onUseAppDefault: () => void;
  onRetry: () => void;
}

/** One scope's ticket settings as a pure view of the read: when changes apply, every
 * setting with its choices, Save once something changed and, on a project page that sets a
 * key, "Use app default". A project without tickets says so. Input: the view props. Output:
 * the page body. */
export function TicketSettingsView(props: TicketSettingsViewProps) {
  const { page, scope, projectId, drafts, busy, note } = props;
  if (page.kind === "loading") return <Skeleton shape="card" />;
  if (page.kind === "not_ready") return <NotReady what="Ticket settings" compact />;
  if (page.kind === "error" && page.code === "project_not_attached") return <Notice tone="info">{NOT_ATTACHED_LINE}</Notice>;
  if (page.kind === "error") return <LoadError message={ticketSettingsRefusal(page)} onRetry={props.onRetry} />;
  assert((scope === "app") === (projectId === null), "only a project page names a project");
  const value = checkedPage(page.value, projectId);
  return <div className="stack ts-page">
    <p className="small muted">{TICKET_EFFECT_LINE}{scope === "project" ? " A value set here wins over the app value for this project only." : ""}</p>
    {value.keys.map((row) => <TicketKeyField key={row.key} row={row} scope={scope} draft={drafts[row.key] ?? draftOf(row)}
      onDraft={(draft) => props.onDraft(row.key, draft)} />)}
    <span className="row">
      {draftsChanged(value.keys, drafts) && <Button variant="primary" size="small" pending={busy} pendingLabel="Saving..." onClick={props.onSave}>Save</Button>}
      {scope === "project" && canUseAppDefault(value) && <Button variant="quiet" size="small" disabled={busy} onClick={props.onUseAppDefault}>Use app default</Button>}
    </span>
    {note && <p className="helper ts-note" role="status">{note}</p>}
  </div>;
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The note after a save that did not store, or "Saved." Input: the result. Output: the
 * line. */
export function saveNote(result: Result<void>): string {
  if (result.kind === "ok") return "Saved.";
  if (result.kind === "not_ready") return "Not ready yet: changing ticket settings isn't in this build.";
  if (result.code === "tracker_settings_stale") return TICKET_SETTINGS_STALE_LINE;
  return ticketSettingsRefusal(result);
}

/** One scope's ticket settings page: reads it, keeps each setting's draft, saves every set key
 * with the version it read, and after a save or a stale refusal reads the page again. Input:
 * the project id, or null for the app's values. */
export function TicketSettingsPanel({ projectId }: { projectId: string | null }) {
  const [page, reload] = useLoad(() => loadTicketSettings(projectId), [projectId]);
  const [drafts, setDrafts] = useState<Partial<Record<TicketKey, KeyDraft>>>({});
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const scope: TicketScope = projectId === null ? "app" : "project";
  const send = async (values: Parameters<typeof saveTicketSettings>[1], version: number) => {
    setBusy(true);
    const result = await saveTicketSettings(projectId, values, version);
    setBusy(false);
    setNote(saveNote(result));
    if (result.kind === "ok" || (result.kind === "error" && result.code === "tracker_settings_stale")) {
      setDrafts({});
      reload();
    }
  };
  const save = () => {
    if (page.kind !== "ok") return;
    const checked = valuesOf(page.value.keys, drafts);
    if ("problem" in checked) {
      setNote(`${keyText(checked.key).label}: ${checked.problem}`);
      return;
    }
    void send(checked.values, page.value.version);
  };
  const useAppDefault = () => {
    if (page.kind === "ok") void send({}, page.value.version);
  };
  return <TicketSettingsView page={page} scope={scope} projectId={projectId} drafts={drafts} busy={busy} note={note}
    onDraft={(key, draft) => { setDrafts((current) => ({ ...current, [key]: draft })); setNote(""); }}
    onSave={save} onUseAppDefault={useAppDefault} onRetry={reload} />;
}

/** Settings, Tickets: the ticket settings every project starts from. Output: the panel. */
export function TicketSettings() {
  return <Panel title="Tickets">
    <p className="secondary">These values apply to every project with tickets until a project sets its own in Project Settings, Tickets.</p>
    <TicketSettingsPanel projectId={null} />
  </Panel>;
}
