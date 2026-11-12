import { useState, type FormEvent } from "react";
import type { Result } from "../../api";
import {
  NOT_ATTACHED_LINE, TICKET_EFFECT_LINE, TICKET_SETTINGS_STALE_LINE, belowLabel, canUseAppDefault, checkedPage, draftOf, draftValue,
  draftsChanged, keyText, loadTicketSettings, optionText, saveTicketSettings, ticketSettingsRefusal, valuesOf,
} from "../../api-ticket-settings";
import type { KeyDraft, TicketKey, TicketScope, TicketSettingRow, TicketSettingsPage } from "../../api-ticket-settings";
import { useLoad, type Loaded } from "../../state";
import { Button, LoadError, Notice, NotReady, RadioCard, SettingLine, SettingSection, Skeleton, TextField } from "../../ui";

/** What one setting's choices need: the row, the page's scope, its draft, whether a save is
 * running and the change handler. */
export interface TicketKeyFieldProps {
  row: TicketSettingRow;
  scope: TicketScope;
  draft: KeyDraft;
  busy: boolean;
  onDraft: (draft: KeyDraft) => void;
}

/** One ticket setting as a Settings line: its label, marked when the choice differs from the
 * value below, and help, then the value below, every option and, for the cycle length, a
 * custom number of days, each with one line saying what it does. Input: the field props.
 * Output: the line. */
export function TicketKeyField({ row, scope, draft, busy, onDraft }: TicketKeyFieldProps) {
  const name = `ts-${row.key}`;
  const text = keyText(row.key);
  const checked = draftValue(row, draft);
  const changed = "value" in checked && checked.value !== null && checked.value !== row.below;
  return <SettingLine wide label={text.label} help={<span id={`${name}-help`}>{text.help}</span>} changed={changed}>
    <fieldset className="field" disabled={busy} aria-describedby={`${name}-help`}>
      <legend className="visually-hidden">{text.label}</legend>
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
    </fieldset>
    {draft.kind === "custom" && <TextField id={`${name}-custom`} label="Days" inputMode="numeric" value={draft.text}
      onValue={(text) => onDraft({ kind: "custom", text })} error={"problem" in checked ? checked.problem : undefined} />}
  </SettingLine>;
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

/** One scope's ticket settings as a pure view of the read, in the Settings section parts: the
 * Ticket rules section saying when changes apply, every setting with its choices, then Save
 * changes, enabled once something changed, and, on a project page that sets a key, "Use app
 * default". A project without tickets says so. Input: the view props. Output: the page body. */
export function TicketSettingsView(props: TicketSettingsViewProps) {
  const { page, scope, projectId, drafts, busy, note } = props;
  if (page.kind === "loading") return <Skeleton shape="line" count={4} />;
  if (page.kind === "not_ready") return <NotReady what="Ticket settings" />;
  if (page.kind === "error" && page.code === "project_not_attached") return <Notice tone="info">{NOT_ATTACHED_LINE}</Notice>;
  if (page.kind === "error") return <LoadError message={ticketSettingsRefusal(page)} onRetry={props.onRetry} />;
  assert((scope === "app") === (projectId === null), "only a project page names a project");
  const value = checkedPage(page.value, projectId);
  const changed = draftsChanged(value.keys, drafts);
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !busy) props.onSave();
  }
  return <form className="pj set-groups" aria-label="Ticket settings" noValidate onSubmit={submit}>
    <SettingSection title="Ticket rules" about={`${TICKET_EFFECT_LINE}${scope === "project" ? " A value set here wins over the app value for this project only." : ""}`}>
      {value.keys.map((row) => <TicketKeyField key={row.key} row={row} scope={scope} draft={drafts[row.key] ?? draftOf(row)} busy={busy}
        onDraft={(draft) => props.onDraft(row.key, draft)} />)}
      <div className="set-body">
        {note && <p className="helper" role="status">{note}</p>}
        <span className="row">
          <Button type="submit" variant="primary" disabled={!changed} pending={busy} pendingLabel="Saving...">Save changes</Button>
          {scope === "project" && canUseAppDefault(value) && <Button variant="quiet" disabled={busy} onClick={props.onUseAppDefault}>Use app default</Button>}
        </span>
      </div>
    </SettingSection>
  </form>;
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

const TICKETS_TOP = <div className="settings-top"><div>
  <h2>Tickets</h2>
  <p>The ticket settings every project with tickets starts from, until a project sets its own in Project Settings, Tickets.</p>
</div></div>;

/** Settings, Tickets: the title in every state, then the ticket settings every project starts
 * from. Output: the tab. */
export function TicketSettings() {
  return <div className="stack">
    {TICKETS_TOP}
    <TicketSettingsPanel projectId={null} />
  </div>;
}
