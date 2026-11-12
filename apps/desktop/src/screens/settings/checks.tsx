import { useState } from "react";
import type { Result } from "../../api";
import {
  approveProfile, checkRows, checksBelowLabel, checksChanged, checksKeyText, checksOptionText, checksRefusal, checksValues, holdoutFiles,
  loadChecksSettings, loadProfile, loadProfiles, profileActions, profileStateText, profileTextProblem, proposeProfile, proposerText, revokeProfile,
  saveChecksSettings, whenText,
} from "../../api-checks";
import type { CheckSpec, ChecksKey, ChecksSettingRow, ChecksSettingsPage, ChecksValue, ProfileRow, StoredProfile } from "../../api-checks";
import { useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, LoadError, NotReady, Notice, Panel, RadioCard, Skeleton, TextArea } from "../../ui";
import { displayText } from "../conversation";
import "../project/tests.css";

/** Which layer a checks page edits: the app's defaults or one project's overrides. */
export type ChecksScope = "app" | "project";

/** The choices a checks page holds before Save: a key set to an option, or null for the layer
 * below; a key not here keeps its saved value. */
export type ChecksPicked = Partial<Record<ChecksKey, ChecksValue | null>>;

const EFFECT_LINE = "Changes apply to check runs queued after you save and to the next Accept.";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** One checks setting: its label and help, then the layer below and every option, each with
 * one line saying what it does. Inputs: the row, the scope, the current pick and the change
 * handler. Output: the fieldset. */
export function ChecksKeyField({ row, scope, picked, onPick }: { row: ChecksSettingRow; scope: ChecksScope; picked: ChecksPicked; onPick: (value: ChecksValue | null) => void }) {
  const name = `cs-${row.key}`;
  const text = checksKeyText(row.key);
  const chosen = Object.hasOwn(picked, row.key) ? picked[row.key] ?? null : row.here;
  return <fieldset className="stack-small tt-key" aria-describedby={`${name}-help`}>
    <legend className="tt-label">{text.label}</legend>
    <p id={`${name}-help`} className="helper">{text.help}</p>
    <RadioCard name={name} value="below" checked={chosen === null} onChecked={() => onPick(null)}
      title={checksBelowLabel(row, scope)} details={[checksOptionText(row.key, row.below).line]} />
    {row.options.map((option) => {
      const words = checksOptionText(row.key, option);
      return <RadioCard key={String(option)} name={name} value={String(option)} checked={chosen === option}
        onChecked={() => onPick(option)} title={words.label} details={[words.line]} />;
    })}
  </fieldset>;
}

/** Everything a checks settings page shows and the actions it offers, without engine calls. */
export interface ChecksSettingsViewProps {
  page: Loaded<ChecksSettingsPage>;
  scope: ChecksScope;
  projectId: string | null;
  picked: ChecksPicked;
  busy: boolean;
  note: string;
  onPick: (key: ChecksKey, value: ChecksValue | null) => void;
  onSave: () => void;
  onUseAppDefault: () => void;
  onRetry: () => void;
}

/** One scope's checks settings as a pure view of the read: when changes apply, every setting
 * with every option, Save once something changed and, on a project page that sets a key,
 * "Use app default". Input: the view props. Output: the page body. */
export function ChecksSettingsView(props: ChecksSettingsViewProps) {
  const { page, scope, projectId, picked, busy, note } = props;
  if (page.kind === "loading") return <Skeleton shape="card" />;
  if (page.kind === "not_ready") return <NotReady what="Check settings" compact />;
  if (page.kind === "error") return <LoadError message={checksRefusal(page)} onRetry={props.onRetry} />;
  assert((scope === "app") === (projectId === null), "only a project page names a project");
  assert(page.value.project_id === projectId, "the page is for the scope shown");
  const rows = page.value.keys;
  return <div className="stack tt-page">
    <p className="small muted">{EFFECT_LINE}{scope === "project" ? " A value set here wins over the app value for this project only." : ""}</p>
    {rows.map((row) => <ChecksKeyField key={row.key} row={row} scope={scope} picked={picked} onPick={(value) => props.onPick(row.key, value)} />)}
    <span className="row">
      {checksChanged(rows, picked) && <Button variant="primary" size="small" pending={busy} pendingLabel="Saving..." onClick={props.onSave}>Save</Button>}
      {scope === "project" && rows.some((row) => row.here !== null) && <Button variant="quiet" size="small" disabled={busy} onClick={props.onUseAppDefault}>Use app default</Button>}
    </span>
    {note && <p className="helper tt-note" role="status">{note}</p>}
  </div>;
}

/** The note after a save: "Saved.", or the refusal in plain words. Input: the result. Output:
 * the line. */
export function saveNote(result: Result<void>): string {
  if (result.kind === "ok") return "Saved.";
  if (result.kind === "not_ready") return "Not ready yet: changing check settings isn't in this build.";
  return checksRefusal(result);
}

/** One scope's checks settings page: reads it, keeps each setting's pick, saves the whole layer
 * with the version it read, and after a save or a stale refusal reads the page again. Input:
 * the project id, or null for the app's defaults. */
export function ChecksSettingsPanel({ projectId }: { projectId: string | null }) {
  const [page, reload] = useLoad(() => loadChecksSettings(projectId), [projectId]);
  const [picked, setPicked] = useState<ChecksPicked>({});
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const scope: ChecksScope = projectId === null ? "app" : "project";
  const send = async (values: Parameters<typeof saveChecksSettings>[1], version: number) => {
    setBusy(true);
    const result = await saveChecksSettings(projectId, values, version);
    setBusy(false);
    setNote(saveNote(result));
    if (result.kind === "ok" || (result.kind === "error" && result.code === "checks_settings_stale")) {
      setPicked({});
      reload();
    }
  };
  const save = () => {
    if (page.kind === "ok") void send(checksValues(page.value.keys, picked), page.value.version);
  };
  const useAppDefault = () => {
    if (page.kind === "ok") void send({}, page.value.version);
  };
  return <ChecksSettingsView page={page} scope={scope} projectId={projectId} picked={picked} busy={busy} note={note}
    onPick={(key, value) => { setPicked((current) => ({ ...current, [key]: value })); setNote(""); }}
    onSave={save} onUseAppDefault={useAppDefault} onRetry={reload} />;
}

/** Settings, Checks: the check settings every project starts from. Output: the panel. */
export function ChecksSettings() {
  return <Panel title="Checks">
    <p className="secondary">These values apply to every project's checks until a project sets its own in Project Settings, Checks.</p>
    <ChecksSettingsPanel projectId={null} />
  </Panel>;
}

/** The project's stored profiles newest first, one selectable row each with its state, who
 * proposed it and how many checks and holdouts it holds. Input: the list, the selected id and
 * the handlers. Output: the list. */
export function ProfilesView({ profiles, selected, onSelect, onRetry }: { profiles: Loaded<{ rows: ProfileRow[]; next: number | null }>; selected: string | null; onSelect: (id: string) => void; onRetry: () => void }) {
  if (profiles.kind === "loading") return <Skeleton shape="row" count={2} />;
  if (profiles.kind === "not_ready") return <NotReady what="Check profiles" compact />;
  if (profiles.kind === "error") return <LoadError message={checksRefusal(profiles)} onRetry={onRetry} />;
  if (profiles.value.rows.length === 0) return <p className="pj-muted">No check profile yet. Write one below, or let an agent propose one for you to approve.</p>;
  return <ul className="tt-rows" aria-label="Check profiles">{profiles.value.rows.map((row) => <li key={row.id}>
    <button type="button" className="tt-row tt-open" aria-current={row.id === selected ? "true" : undefined} onClick={() => onSelect(row.id)}>
      <span className="pj-mono pj-small">{row.id.slice(0, 12)}</span>
      <span className="tt-title">{profileStateText(row.state)}</span>
      <span className="pj-small pj-secondary">{proposerText(displayText(row.proposed_by))}, {whenText(row.proposed_ms)}</span>
      <span className="pj-small">{row.checks === 1 ? "1 check" : `${row.checks} checks`}, {row.holdouts === 1 ? "1 holdout" : `${row.holdouts} holdouts`}</span>
    </button>
  </li>)}</ul>;
}

const MONO: readonly string[] = ["id", "argv", "cwd", "report"];

function CheckLine({ check }: { check: CheckSpec }) {
  return <li className="tt-profile-check">
    <dl className="tt-covered">
      {checkRows(check).map((row) => <div key={row.field} className="tt-covered-row">
        <dt>{row.label}</dt>
        <dd className={MONO.includes(row.field) ? "pj-mono" : undefined}>{displayText(row.value)}</dd>
      </div>)}
    </dl>
  </li>;
}

/** One stored profile in full as the owner sees it: state, who proposed it and when, every
 * check, every holdout check with its files as path and digest only, the protected paths,
 * and Approve or Revoke as its state allows. Input: the profile and the handlers. Output:
 * the detail. */
export function ProfileDetailView({ profile, busy, onApprove, onRevoke }: { profile: StoredProfile; busy: boolean; onApprove: () => void; onRevoke: () => void }) {
  const actions = profileActions(profile.state);
  const body = profile.profile;
  return <section className="tt-block tt-profile" aria-label="Check profile">
    <div className="pj-row between">
      <h4>{profileStateText(profile.state)}</h4>
      <span className="pj-row">
        {actions.approve && <Button variant="primary" size="small" disabled={busy} onClick={onApprove}>Approve</Button>}
        {actions.revoke && <Button variant="quiet" size="small" disabled={busy} onClick={onRevoke}>Revoke</Button>}
      </span>
    </div>
    <p className="pj-small pj-secondary">
      <span className="pj-mono">{profile.id}</span>, proposed by {proposerText(displayText(profile.proposed_by))} {whenText(profile.proposed_ms)}
      {profile.approved_ms === null ? "" : `, approved ${whenText(profile.approved_ms)}`}{profile.revoked_ms === null ? "" : `, revoked ${whenText(profile.revoked_ms)}`}
      {profile.question === null ? "" : `, asked in question ${profile.question}`}
    </p>
    <h4>Checks</h4>
    {body.checks.length === 0 ? <p className="pj-muted pj-small">No checks.</p> : <ul className="tt-lines">{body.checks.map((check) => <CheckLine key={check.id} check={check} />)}</ul>}
    {body.holdouts.length > 0 && <>
      <h4>Holdout checks</h4>
      <p className="pj-small pj-secondary">Agents see only whether a holdout passed. Its files stay inside the engine; here they are named by path and digest.</p>
      <ul className="tt-lines">{body.holdouts.map((holdout) => <li key={holdout.check.id}>
        <ul className="tt-lines"><CheckLine check={holdout.check} /></ul>
        <ul className="tt-lines">{holdoutFiles(holdout.files).map((file) => <li key={file.path} className="pj-small">
          <span className="pj-mono">{displayText(file.path)}</span> <span className="pj-mono pj-muted">{file.digest}</span>
        </li>)}</ul>
      </li>)}</ul>
    </>}
    {body.protected_paths.length > 0 && <p className="pj-small">Protected paths: <span className="pj-mono">{body.protected_paths.map(displayText).join(", ")}</span></p>}
  </section>;
}

/** An Approve or Revoke the owner is asked to confirm. */
export type ProfileAsking = { kind: "approve" | "revoke"; id: string } | null;

/** The desktop confirm before Approve or Revoke, saying what follows. Input: what is asked,
 * busy and the handlers. Output: the dialog. */
export function ProfileConfirm({ asking, busy, onConfirm, onCancel }: { asking: ProfileAsking; busy: boolean; onConfirm: () => void; onCancel: () => void }) {
  const approve = asking?.kind === "approve";
  return <ConfirmDialog open={asking !== null} title={approve ? "Approve this profile?" : "Revoke this profile?"} confirmLabel={approve ? "Approve" : "Revoke"}
    pending={busy} onConfirm={onConfirm} onCancel={onCancel}>
    <p>{approve ? "Check runs from now on use exactly these commands, network and timeouts. Runs under another profile stop counting for Accept."
      : "No check runs until you approve another profile, and earlier runs stop counting for Accept."}</p>
  </ConfirmDialog>;
}

function ProfileEditor({ projectId, onProposed }: { projectId: string; onProposed: (id: string) => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const problem = text === "" ? null : profileTextProblem(text);
  const propose = async () => {
    const blocked = profileTextProblem(text);
    if (blocked !== null) { setNote(blocked); return; }
    setBusy(true);
    const result = await proposeProfile(projectId, text);
    setBusy(false);
    if (result.kind === "ok") { setText(""); setNote("Proposed. Approve it above to use it."); onProposed(result.value); }
    else setNote(result.kind === "error" ? checksRefusal(result) : "Not ready yet: proposing profiles isn't in this build.");
  };
  return <div className="pj-stack-s">
    <TextArea id="cs-profile" label="New profile (JSON)" value={text} onValue={(value) => { setText(value); setNote(""); }} rows={10}
      helper="The checks this project runs: each with its command, folder, report format, timeout, network and when it runs. This window doesn't store holdout files: a holdout names files already stored in the engine, by digest." error={problem ?? undefined} />
    <span className="row"><Button size="small" pending={busy} pendingLabel="Proposing..." disabled={text === "" || problem !== null} onClick={() => void propose()}>Propose</Button></span>
    {note && <p className="helper" role="status">{note}</p>}
  </div>;
}

function ProfileDetail({ projectId, id, onChanged }: { projectId: string; id: string; onChanged: () => void }) {
  const [profile, reload] = useLoad(() => loadProfile(projectId, id), [projectId, id]);
  const [asking, setAsking] = useState<ProfileAsking>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const confirm = async () => {
    if (asking === null) return;
    setBusy(true);
    const result = asking.kind === "approve" ? await approveProfile(projectId, asking.id) : await revokeProfile(projectId, asking.id);
    setBusy(false);
    setAsking(null);
    setNote(result.kind === "ok" ? (asking.kind === "approve" ? "Approved." : "Revoked.") : result.kind === "error" ? checksRefusal(result) : "Not ready yet.");
    reload();
    onChanged();
  };
  if (profile.kind === "loading") return <Skeleton shape="card" />;
  if (profile.kind === "not_ready") return <NotReady what="Check profiles" compact />;
  if (profile.kind === "error") return <LoadError message={checksRefusal(profile)} onRetry={reload} />;
  return <>
    <ProfileDetailView profile={profile.value} busy={busy} onApprove={() => setAsking({ kind: "approve", id })} onRevoke={() => setAsking({ kind: "revoke", id })} />
    {note && <p className="helper" role="status">{note}</p>}
    <ProfileConfirm asking={asking} busy={busy} onConfirm={() => void confirm()} onCancel={() => setAsking(null)} />
  </>;
}

/** Project Settings, Checks: this project's overrides of the app's check settings, its stored
 * profiles with Approve and Revoke behind a confirm, and an editor to propose a new profile.
 * Input: the project id. Output: the section. */
export function ProjectChecks({ projectId }: { projectId: string }) {
  const [profiles, reload] = useLoad(() => loadProfiles(projectId, null), [projectId]);
  const [selected, setSelected] = useState<string | null>(null);
  return <div className="pj-stack">
    <h2>Checks</h2>
    <ChecksSettingsPanel projectId={projectId} />
    <h3>Check profiles</h3>
    <Notice tone="info">A check runs only under a profile you approved. Agents can propose one; nothing runs until you approve it here.</Notice>
    <ProfilesView profiles={profiles} selected={selected} onSelect={setSelected} onRetry={reload} />
    {profiles.kind === "ok" && profiles.value.next !== null && <p className="pj-small pj-muted">Older profiles aren't listed.</p>}
    {selected !== null && <ProfileDetail key={selected} projectId={projectId} id={selected} onChanged={reload} />}
    <ProfileEditor projectId={projectId} onProposed={(id) => { reload(); setSelected(id); }} />
  </div>;
}
