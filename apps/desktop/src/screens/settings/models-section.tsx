import { useEffect, useState } from "react";
import { poolSummaries, type Result } from "../../api";
import type { SettingScope } from "../../api-harness-settings";
import {
  ALLOWED_MAX, FALLBACK_HELP, FALLBACK_OPTIONS, MODEL_BYTES_MAX, MODEL_STALE_LINE, NOT_SUPPORTED, NO_TEST_LAUNCH, PICK_HELP, PICK_OPTIONS,
  SLOT_HELP, SLOT_LABELS, addModel, addedLine, allowedChange, allowedDraft, canResetModel, catalogLine, choiceChange, choiceDraft, choiceOptions,
  loadModelRules, loadModels, modelChoiceRefusal, modelSourceLine, modelsNotReady, modelsStale, originWord, ownWord, passedOverLine,
  removeModel, removedLine, resetModelChoice, resetModelLabel, setModelChoice, setModelRule, testModel, tickable,
  toggleAllowed, uncheckedLine, valueText,
} from "../../api-model-choice";
import type { FallbackRule, ModelCandidate, ModelRow, ModelRules, ModelSlot, ModelTested, ModelsPage, PickRule } from "../../api-model-choice";
import { useLoad, type Loaded } from "../../state";
import { Button, Checkbox, LoadError, NotReady, Notice, Segmented, Select, SettingLine, SettingSection, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import "./models-section.css";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The drafts of one Models section: a choice for the master and agent slots, ticks for the
 * allowed list; a missing slot shows what is saved. */
export interface ModelDrafts {
  master?: string;
  agent?: string;
  allowed?: string[];
}

/** One slot's line and what it can do, without engine calls. */
interface SlotProps {
  row: ModelRow;
  candidates: readonly ModelCandidate[];
  scope: SettingScope;
  draft: string | string[] | undefined;
  busy: boolean;
  note: string;
  onDraft: (draft: string | string[]) => void;
  onSave: () => void;
  onReset: () => void;
}

function Actions({ row, scope, label, changed, busy, onSave, onReset }: { row: ModelRow; scope: SettingScope; label: string; changed: boolean; busy: boolean; onSave: () => void; onReset: () => void }) {
  const reset = resetModelLabel(row, scope);
  if (!changed && !canResetModel(row)) return null;
  return <span className="row">
    {changed && <Button variant="primary" size="small" pending={busy} pendingLabel="Saving..." aria-label={`Save ${label}`} onClick={onSave}>Save</Button>}
    {canResetModel(row) && <Button variant="quiet" size="small" disabled={busy && changed} pending={busy && !changed} pendingLabel="Clearing..." aria-label={`${reset}, ${label}`} onClick={onReset}>{reset}</Button>}
  </span>;
}

function SlotLine({ row, candidates, scope, draft, busy, note, onDraft, onSave, onReset }: SlotProps) {
  const label = SLOT_LABELS[row.slot];
  const passed = passedOverLine(row, candidates);
  const lines = <>
    {passed && <p className="helper">{passed}</p>}
    {note && <p className="helper" role="status">{note}</p>}
  </>;
  const changedHere = row.source === (scope === "app" ? "app" : "project");
  if (row.access.kind !== "editable") {
    return <SettingLine label={label} help={SLOT_HELP[row.slot]}>
      <div className="models-ctl">
        <span className="mono">{valueText(row.effective)}</span>
        <p className="helper">{displayText(row.access.reason)}</p>
        {lines}
        <Actions row={row} scope={scope} label={label} changed={false} busy={busy} onSave={onSave} onReset={onReset} />
      </div>
    </SettingLine>;
  }
  if (row.slot === "allowed") {
    assert(draft === undefined || Array.isArray(draft), "an allowed list draft is a list");
    const ticks = draft ?? allowedDraft(row);
    const changed = allowedChange(row, candidates, ticks).kind !== "none";
    return <SettingLine label={label} help={SLOT_HELP.allowed} changed={changedHere} wide>
      <div className="models-ctl">
        <fieldset className="models-allowed" aria-label={label}>
          {candidates.map((candidate) => {
            const state = originWord(candidate.origin);
            return <Checkbox key={candidate.id} checked={ticks.includes(candidate.id)} disabled={!tickable(candidate, ticks)}
              label={state ? <>{`${displayText(candidate.id)} `}<span className="small muted">{state}</span></> : displayText(candidate.id)}
              onChecked={(on) => onDraft(toggleAllowed(ticks, candidate.id, on, candidates))} />;
          })}
        </fieldset>
        <span className="small muted">{ticks.length} of at most {ALLOWED_MAX} ticked. {modelSourceLine(row, scope)}</span>
        {lines}
        <Actions row={row} scope={scope} label={label} changed={changed} busy={busy} onSave={onSave} onReset={onReset} />
      </div>
    </SettingLine>;
  }
  assert(draft === undefined || typeof draft === "string", "a model choice draft is one id");
  const choice = draft ?? choiceDraft(row, candidates);
  const changed = choiceChange(row, candidates, choice).kind !== "none";
  const id = `models-${row.slot}`;
  return <SettingLine label={label} help={SLOT_HELP[row.slot]} changed={changedHere} htmlFor={id}>
    <div className="models-ctl">
      <Select id={id} label={label} hideLabel value={choice} options={choiceOptions(row, candidates, scope)} onValue={onDraft} />
      <span className="small muted">{modelSourceLine(row, scope)}</span>
      {lines}
      <Actions row={row} scope={scope} label={label} changed={changed} busy={busy} onSave={onSave} onReset={onReset} />
    </div>
  </SettingLine>;
}

/** Everything the Models section shows and the actions it offers, without engine calls. */
export interface ModelsViewProps {
  page: Loaded<ModelsPage>;
  name: string;
  scope: SettingScope;
  drafts: Readonly<ModelDrafts>;
  busy: ModelSlot | null;
  notes: Readonly<Partial<Record<ModelSlot, string>>>;
  onDraft: (slot: ModelSlot, draft: string | string[]) => void;
  onSave: (row: ModelRow) => void;
  onReset: (row: ModelRow) => void;
  onRetry: () => void;
}

/** One harness's Models section as a pure view of the page read: what the model list was
 * checked against, then the master model, the agent default and the allowed list, each with
 * where its value comes from, Save when it changed and its reset when one is stored. A slot
 * the harness does not support shows the reason and no control. Input: the view props.
 * Output: the section. */
export function ModelsView(props: ModelsViewProps) {
  const { page, name, scope, drafts, busy, notes } = props;
  if (page.kind === "loading") return <Skeleton shape="row" count={3} />;
  if (page.kind === "not_ready" || modelsNotReady(page)) return <NotReady what="Model choice" compact />;
  if (page.kind === "error") return <LoadError message={modelChoiceRefusal(page)} onRetry={props.onRetry} />;
  const value = page.value;
  assert((scope === "project") === (value.project_id !== null), "a project page names its project");
  const about = scope === "project" ? "A model set here wins over the app value for this project only." : "Changes apply the next time the master or an agent starts with this harness.";
  const unchecked = uncheckedLine(value.models);
  return <SettingSection title="Models" about={<>{catalogLine(value, name)} {unchecked ? `${unchecked} ` : ""}{about}</>}>
    {value.models.rows.map((row) => <SlotLine key={row.slot} row={row} candidates={value.models.candidates} scope={scope}
      draft={drafts[row.slot]} busy={busy === row.slot} note={notes[row.slot] ?? ""}
      onDraft={(draft) => props.onDraft(row.slot, draft)} onSave={() => props.onSave(row)} onReset={() => props.onReset(row)} />)}
  </SettingSection>;
}

/** The line beside an owner id while its test launch runs. */
export const TESTING_LINE = "Testing. This can take up to 2 minutes.";

/** The note after a test launch, in the TUI's words: what it decided, with the reason an id
 * still waits as the engine gave it, or why nothing was tested. Inputs: the id and the
 * result. Output: the line. */
export function testNote(model: string, result: Result<ModelTested>): string {
  const id = displayText(model);
  if (result.kind !== "ok") return `Could not test ${id}. ${modelNote(result)}`;
  switch (result.value.tested) {
    case "verified": return `${id} passed its test launch. You can now choose it, tick it and launch it.`;
    case "failed": return `${id} failed its test launch. The provider refused this model. It stays listed and can't be used.`;
    case "pending": return [`${id} still waits for its test launch.`, displayText(result.value.reason ?? "")].filter(Boolean).join(" ");
  }
}

/** The pool question of one id's test launch, without engine calls: every pool as a radio,
 * Run test once one is picked, or the line that there is no pool yet. Input: the props.
 * Output: the question. */
export function TestPoolView({ model, pools, picked, busy, onPick, onRun, onCancel }: {
  model: string; pools: readonly { id: string; name: string }[]; picked: string | null; busy: boolean;
  onPick: (pool: string) => void; onRun: () => void; onCancel: () => void;
}) {
  return <div className="stack-small">
    <fieldset className="pr-ai-pools">
      <legend>{`Which pool runs the test launch of ${displayText(model)}?`}</legend>
      <p className="small muted">One short run of a fixed prompt on an account of the pool you pick, with this id as its model.</p>
      {pools.length === 0 && <Notice>No pool yet. Add one under Pools, then try again.</Notice>}
      {pools.map((pool) => <label key={pool.id} className="pr-ai-pool">
        <input type="radio" name="models-test-pool" value={pool.id} checked={picked === pool.id} disabled={busy} onChange={() => onPick(pool.id)} />
        <span>{displayText(pool.name)}</span>
      </label>)}
    </fieldset>
    <span className="row">
      <Button variant="primary" disabled={picked === null} pending={busy} pendingLabel="Testing..." onClick={onRun}>Run test</Button>
      <Button disabled={busy} onClick={onCancel}>Cancel</Button>
    </span>
  </div>;
}

/** The owner's own model ids for one harness and the field to add one, without engine
 * calls: each id with its state, a Test for a pending id, and a remove; an empty field adds
 * nothing. Where the engine says the harness has no test launch, Add and a pending id say
 * Not supported, as the TUI does (MUG2-L1). Input: the props. Output: the section. */
export function OwnIdsView({ candidates, field, busy, note, onField, onAdd, onRemove, testable, testing = null, onTest = () => {} }: {
  candidates: readonly ModelCandidate[]; field: string; busy: string | null; note: string;
  onField: (field: string) => void; onAdd: () => void; onRemove: (id: string) => void;
  testable: boolean; testing?: string | null; onTest?: (id: string) => void;
}) {
  const own = candidates.filter((candidate) => candidate.origin !== "catalog");
  return <SettingSection title="Your model ids" about="Add a model id this harness's list doesn't have yet. It waits for a test launch and can be chosen once that passes.">
    {testable
      ? <SettingLine label="Model id" htmlFor="models-add-id" help="Letters, digits and - _ . : / @ +, starting with a letter or digit.">
        <span className="models-add">
          <input id="models-add-id" className="input mono" value={field} maxLength={MODEL_BYTES_MAX} autoComplete="off" spellCheck={false}
            onChange={(event) => onField(event.target.value)} />
          <Button variant="secondary" size="small" pending={busy === "add"} pendingLabel="Adding..." disabled={field.trim() === ""} onClick={onAdd}>Add</Button>
        </span>
      </SettingLine>
      : <SettingLine label="Model id" help={NO_TEST_LAUNCH}><span className="muted">{`${NOT_SUPPORTED}.`}</span></SettingLine>}
    {own.length === 0 && <p className="small muted">You haven't added a model id to this harness.</p>}
    {own.map((candidate) => <SettingLine key={candidate.id} label={displayText(candidate.id)} help={testing === candidate.id ? TESTING_LINE : ownWord(candidate.origin, testable)}>
      <span className="row">
        {testable && candidate.origin === "pending" && <Button variant="secondary" size="small" pending={testing === candidate.id} pendingLabel="Testing..."
          disabled={testing !== null} aria-label={`Test ${candidate.id}`} onClick={() => onTest(candidate.id)}>Test</Button>}
        <Button variant="quiet" size="small" pending={busy === candidate.id} pendingLabel="Removing..." disabled={testing === candidate.id} aria-label={`Remove ${candidate.id}`} onClick={() => onRemove(candidate.id)}>Remove</Button>
      </span>
    </SettingLine>)}
    {note && <p className="helper" role="status">{note}</p>}
  </SettingSection>;
}

/** The owner's two app-wide model rules, without engine calls: what the master may pick and
 * what a start does when no account can run the model, each with every option and what the
 * one in force does. Input: the props. Output: the section. */
export function ModelRulesView({ rules, busy, note, onRule, onRetry }: {
  rules: Loaded<ModelRules>; busy: "pick" | "fallback" | null; note: string;
  onRule: (rule: "pick" | "fallback", value: PickRule | FallbackRule) => void; onRetry: () => void;
}) {
  if (rules.kind === "loading") return <Skeleton shape="row" count={2} />;
  if (rules.kind === "not_ready" || modelsNotReady(rules)) return <NotReady what="Model rules" compact />;
  if (rules.kind === "error") return <LoadError message={modelChoiceRefusal(rules)} onRetry={onRetry} />;
  const value = rules.value;
  return <SettingSection title="Model rules" about="These apply to every harness and every project.">
    <SettingLine label="What the master may pick" help={PICK_HELP[value.pick]} changed={value.pick !== "strict"}>
      <Segmented<PickRule> label="What the master may pick" value={value.pick} options={PICK_OPTIONS} onValue={(next) => busy === null && onRule("pick", next)} />
    </SettingLine>
    <SettingLine label="When no account can run the model" help={FALLBACK_HELP[value.fallback]} changed={value.fallback !== "notice"}>
      <Segmented<FallbackRule> label="When no account can run the model" value={value.fallback} options={FALLBACK_OPTIONS} onValue={(next) => busy === null && onRule("fallback", next)} />
    </SettingLine>
    {note && <p className="helper" role="status">{note}</p>}
  </SettingSection>;
}

/** The note after a change that did not store. Input: the result. Output: the line, or "". */
export function modelNote(result: Result<unknown>): string {
  if (result.kind === "ok") return "";
  if (result.kind === "not_ready" || modelsNotReady(result)) return "Not ready yet: model choice isn't in this build.";
  if (modelsStale(result)) return MODEL_STALE_LINE;
  return modelChoiceRefusal(result);
}

function AppOnly({ harness, page, onChanged }: { harness: string; page: Loaded<ModelsPage>; onChanged: () => void }) {
  const [rules, reloadRules] = useLoad(loadModelRules, []);
  const [field, setField] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [ruleBusy, setRuleBusy] = useState<"pick" | "fallback" | null>(null);
  const [ruleNote, setRuleNote] = useState("");
  const [asking, setAsking] = useState<{ model: string; pools: { id: string; name: string }[]; picked: string | null } | null>(null);
  const [testing, setTesting] = useState<string | null>(null);

  async function askPool(model: string) {
    const pools = await poolSummaries();
    if (pools.kind !== "ok") {
      setNote("The pools couldn't be read. Try again.");
      return;
    }
    setNote("");
    setAsking({ model, pools: pools.value.map(({ id, name }) => ({ id, name })), picked: null });
  }

  async function runTest() {
    if (asking === null || asking.picked === null) return;
    const { model, picked } = asking;
    setTesting(model);
    const result = await testModel(harness, model, picked);
    setTesting(null);
    setAsking(null);
    setNote(testNote(model, result));
    if (result.kind === "ok") onChanged();
  }

  async function add() {
    const id = field.trim();
    if (id === "") return;
    setBusy("add");
    const result = await addModel(harness, id);
    setBusy(null);
    setNote(result.kind === "ok" ? addedLine(result.value, id) : modelNote(result));
    if (result.kind !== "ok") return;
    setField("");
    onChanged();
  }

  async function remove(id: string) {
    setBusy(id);
    const result = await removeModel(harness, id);
    setBusy(null);
    setNote(result.kind === "ok" ? removedLine(result.value, id) : modelNote(result));
    if (result.kind === "ok") onChanged();
  }

  async function rule(name: "pick" | "fallback", value: PickRule | FallbackRule) {
    if (rules.kind !== "ok") return;
    setRuleBusy(name);
    const result = await setModelRule(name, value, name === "pick" ? rules.value.pick_version : rules.value.fallback_version);
    setRuleBusy(null);
    setRuleNote(modelNote(result));
    if (result.kind === "ok" || modelsStale(result)) reloadRules();
  }

  return <>
    {page.kind === "ok" && <OwnIdsView candidates={page.value.models.candidates} field={field} busy={busy} note={note}
      onField={setField} onAdd={() => void add()} onRemove={(id) => void remove(id)}
      testable={page.value.models.testable} testing={testing} onTest={(id) => void askPool(id)} />}
    {asking !== null && <TestPoolView model={asking.model} pools={asking.pools} picked={asking.picked} busy={testing !== null}
      onPick={(pool) => setAsking({ ...asking, picked: pool })} onRun={() => void runTest()} onCancel={() => setAsking(null)} />}
    <ModelRulesView rules={rules} busy={ruleBusy} note={ruleNote} onRule={(name, value) => void rule(name, value)} onRetry={reloadRules} />
  </>;
}

/** One harness's Models block, app-wide or for one project: reads the section, keeps each
 * slot's draft and sends a set or reset with the version it read, reading again after a
 * stale refusal. The app page also lists the owner's model ids and the model rules. Inputs:
 * the harness id, its name and the project id, or null for the app. */
export function ModelsSection({ harness, name, projectId }: { harness: string; name: string; projectId: string | null }) {
  const [page, reload] = useLoad(() => loadModels(harness, projectId), [harness, projectId]);
  const [drafts, setDrafts] = useState<ModelDrafts>({});
  const [notes, setNotes] = useState<Partial<Record<ModelSlot, string>>>({});
  const [busy, setBusy] = useState<ModelSlot | null>(null);
  useEffect(() => { setDrafts({}); setNotes({}); }, [harness, projectId]);

  function settle(slot: ModelSlot, result: Result<void>) {
    setBusy(null);
    setNotes((old) => ({ ...old, [slot]: modelNote(result) }));
    if (result.kind !== "ok" && !modelsStale(result)) return;
    setDrafts((old) => ({ ...old, [slot]: undefined }));
    reload();
  }

  async function save(row: ModelRow) {
    if (page.kind !== "ok") return;
    const candidates = page.value.models.candidates;
    if (row.slot === "allowed") {
      const change = allowedChange(row, candidates, drafts.allowed ?? allowedDraft(row));
      if (change.kind !== "set") return;
      setBusy(row.slot);
      return settle(row.slot, await setModelChoice(harness, projectId, row.slot, change.value, row.version));
    }
    const change = choiceChange(row, candidates, drafts[row.slot] ?? choiceDraft(row, candidates));
    if (change.kind === "none") return;
    setBusy(row.slot);
    settle(row.slot, change.kind === "reset"
      ? await resetModelChoice(harness, projectId, row.slot, row.version)
      : await setModelChoice(harness, projectId, row.slot, change.value, row.version));
  }

  async function reset(row: ModelRow) {
    setBusy(row.slot);
    settle(row.slot, await resetModelChoice(harness, projectId, row.slot, row.version));
  }

  return <div className="pj set-groups models-section">
    <ModelsView page={page} name={name} scope={projectId === null ? "app" : "project"} drafts={drafts} busy={busy} notes={notes}
      onDraft={(slot, draft) => setDrafts((old) => ({ ...old, [slot]: draft }))}
      onSave={(row) => void save(row)} onReset={(row) => void reset(row)} onRetry={reload} />
    {projectId === null && <AppOnly harness={harness} page={page} onChanged={reload} />}
  </div>;
}
