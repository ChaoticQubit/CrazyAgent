import { useEffect, useRef, useState } from "react";
import { listSkills, type Result } from "../api";
import {
  DEFAULT_INSTRUCTION_SETTINGS, INSTRUCTION_CHOICES, PROCESS_TEXT_BYTES_MAX, defaultProcess, importGlobalInstructions, loadInstructionSettings,
  loadProcess, processMessage, processTextError, saveInstructionSettings, saveProcess, savedMessage, skillSegments, textBytes,
  type InstructionSettings, type SkillEntry,
} from "../api-process";
import { useAppSession, useLoad, type Loaded } from "../state";
import { Button, ConfirmDialog, Dialog, LoadError, NotReady, Notice, Panel, Segmented, Skeleton, Switch, TextArea } from "../ui";
import { navigate } from "../ui/router";
import { SkillTag, skillEntries } from "./skill-tag";
import "./process.css";

/** Where an instructions text applies: every project, or one project. */
export type ProcessScope = { kind: "app" } | { kind: "project"; id: string; name: string };

/** A seed text the owner opened for a preview before it goes into the editor. */
export interface ProcessPreview {
  source: "import" | "default";
  text: string;
}

/** Shows instructions text with each catalog skill name as a tag: hovering shows its one-line
 * description and clicking opens that skill in Settings, or hands it to `onOpen` when the
 * page decides first. Inputs: the text, the catalog and the optional opener. */
export function TaggedText({ text, skills, onOpen }: { text: string; skills: readonly SkillEntry[]; onOpen?: (skill: SkillEntry) => void }) {
  return <div className="process-text">
    {skillSegments(text, skills).map((segment, index) => segment.kind === "text"
      ? <span key={index}>{segment.text}</span>
      : <SkillTag key={index} name={segment.name} skills={[segment]} onOpen={onOpen} />)}
  </div>;
}

/** The draft to show once the saved text is read again: the new text while the owner left
 * the last read text untouched, else the owner's unsaved draft, so no reply or save made
 * elsewhere replaces typing. Inputs: the draft, the text last read (null before the first
 * read) and the text just read. Output: the draft. */
export function keptDraft(draft: string, loaded: string | null, saved: string): string {
  return loaded === null || draft === loaded ? saved : draft;
}

/** Writes a byte count the way the Process pages show sizes. Input: bytes. Output: the words. */
export function byteSize(bytes: number): string {
  return bytes < 1024 ? `${bytes} bytes` : `${(bytes / 1024).toFixed(1)} KB`;
}

/** The Process page as a pure view: the editor with Save, the text with skill tags, and at
 * app level the two seeds, each opening a preview first. Inputs: where it applies, the saved
 * text, the draft, the skill catalog, the last failure, what is running, the preview, the
 * skill the owner asked to open over unsaved text and the actions. The editor is read-only
 * while saving. Nothing reads the owner's files or saves without the matching click. */
export function ProcessView({ scope, saved, draft, skills, failure, busy, preview, leaving = null, onDraft, onSave, onImport, onDefault, onUsePreview, onClosePreview, onRetry, onOpenSkill, onLeave, onStay }: {
  scope: ProcessScope;
  saved: Loaded<string>;
  draft: string;
  skills: readonly SkillEntry[];
  failure: string;
  busy: "save" | "import" | "default" | null;
  preview: ProcessPreview | null;
  leaving?: SkillEntry | null;
  onDraft: (text: string) => void;
  onSave: () => void;
  onImport: () => void;
  onDefault: () => void;
  onUsePreview: () => void;
  onClosePreview: () => void;
  onRetry: () => void;
  onOpenSkill?: (skill: SkillEntry) => void;
  onLeave?: () => void;
  onStay?: () => void;
}) {
  const intro = scope.kind === "app"
    ? "Instructions every master and sub-agent gets, in every project. A project's own instructions come after these."
    : `Instructions every agent in ${scope.name} gets, after the app-wide ones. Kept in the project's .crazyagent/instructions.md, which git ignores.`;
  if (saved.kind === "loading") return <Skeleton shape="card" count={2} />;
  if (saved.kind === "not_ready") return <NotReady what="Instructions" />;
  if (saved.kind === "error") return <LoadError message={saved.message} onRetry={onRetry} />;
  const invalid = processTextError(draft);
  const changed = draft !== saved.value;
  const seeds = scope.kind === "app" && <span className="row">
    <Button icon="upload" pending={busy === "import"} pendingLabel="Reading..." disabled={busy !== null} onClick={onImport}>Import my global instructions</Button>
    <Button icon="sparkles" pending={busy === "default"} pendingLabel="Reading..." disabled={busy !== null} onClick={onDefault}>Use the CrazyAgent default process</Button>
  </span>;
  return <div className="stack">
    <p className="secondary">{intro}</p>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {seeds}
    <Panel title="Instructions">
      <form className="stack-small" onSubmit={(event) => { event.preventDefault(); if (!invalid && changed) onSave(); }}>
        <TextArea id={`process-${scope.kind}`} label="Instructions for agents" value={draft} onValue={onDraft} rows={16} error={invalid} readOnly={busy === "save"}
          helper={`${byteSize(textBytes(draft))} of ${byteSize(PROCESS_TEXT_BYTES_MAX)}${changed ? ". Not saved yet." : ""}`} />
        <span className="row">
          <Button type="submit" variant="primary" icon="check" pending={busy === "save"} pendingLabel="Saving..."
            disabled={Boolean(invalid) || !changed || busy !== null}>Save</Button>
          {changed && <Button disabled={busy !== null} onClick={() => onDraft(saved.value)}>Discard edits</Button>}
        </span>
      </form>
    </Panel>
    {draft.trim() && <Panel title="As agents read it"><TaggedText text={draft} skills={skills} onOpen={onOpenSkill} /></Panel>}
    <Dialog open={preview !== null} wide title={preview?.source === "import" ? "Your global instructions" : "CrazyAgent default process"}
      onClose={onClosePreview}
      footer={<>
        <Button onClick={onClosePreview}>Cancel</Button>
        <Button variant="primary" icon="check" onClick={onUsePreview}>Put in the editor</Button>
      </>}>
      <p className="secondary">{preview?.source === "import"
        ? "Read from ~/.claude/CLAUDE.md. It replaces the editor's text; nothing is saved until you click Save."
        : "It replaces the editor's text; nothing is saved until you click Save."}</p>
      {preview && <TaggedText text={preview.text} skills={skills} onOpen={onOpenSkill} />}
    </Dialog>
    <ConfirmDialog open={leaving !== null} title={`Open ${leaving?.name ?? "the skill"} without saving?`} confirmLabel="Open skill"
      cancelLabel="Keep editing" onConfirm={() => onLeave?.()} onCancel={() => onStay?.()}>
      Your instructions changes will be lost.
    </ConfirmDialog>
  </div>;
}

/** A project's agent settings as a pure view: which instruction files its agents get and
 * whether headless Claude runs keep their transcript, each saved on its own change. Settings
 * that could not be read say why and show the defaults, so a change replaces them.
 * Inputs: the read, whether a save runs, the last save failure and the save action. */
export function AgentSettingsView({ settings, busy, failure, onSave }: {
  settings: Loaded<InstructionSettings>;
  busy: boolean;
  failure: string;
  onSave: (settings: InstructionSettings) => void;
}) {
  if (settings.kind === "loading") return <Skeleton shape="card" count={1} />;
  if (settings.kind === "not_ready") return <NotReady what="Agent settings" />;
  const current = settings.kind === "ok" ? settings.value : DEFAULT_INSTRUCTION_SETTINGS;
  const choice = INSTRUCTION_CHOICES.find((option) => option.value === current.files) ?? INSTRUCTION_CHOICES[0];
  const save = (next: InstructionSettings) => { if (!busy) onSave(next); };
  return <Panel title="Agent settings">
    <div className="stack-small">
      {settings.kind === "error" && <Notice tone="danger" alert>{processMessage(settings)}</Notice>}
      {failure && <Notice tone="danger" alert>{failure}</Notice>}
      <span>Agent instruction files</span>
      <Segmented label="Agent instruction files" value={current.files}
        options={INSTRUCTION_CHOICES.map(({ value, label }) => ({ value, label }))}
        onValue={(files) => save({ ...current, files })} />
      <p className="secondary">{choice.note}</p>
      <Switch id="agent-transcripts" label="Keep transcripts for headless Claude runs" checked={current.transcripts}
        disabledReason={busy ? "Saving..." : undefined} onChecked={(transcripts) => save({ ...current, transcripts })} />
      <p className="secondary">{current.transcripts
        ? "Headless Claude runs keep their transcript in the agent's own home."
        : "Headless Claude runs keep no transcript."}</p>
    </div>
  </Panel>;
}

/** A project's agent settings: reads them from the engine and saves each change at once.
 * Input: the project id. */
export function AgentSettings({ projectId }: { projectId: string }) {
  const session = useAppSession();
  const [settings, reload] = useLoad(() => loadInstructionSettings(projectId), [projectId]);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const store = async (next: InstructionSettings) => {
    setFailure("");
    setBusy(true);
    const result = session.noteResult(await saveInstructionSettings(projectId, next));
    setBusy(false);
    if (result.kind === "ok") {
      session.showToast("Agent settings saved. Agents started from now on follow them.");
      reload();
    } else setFailure(processMessage(result));
  };
  return <AgentSettingsView settings={settings} busy={busy} failure={failure} onSave={(next) => void store(next)} />;
}

/** The skill catalog the tags name, from the engine's skill list. Input: the read, whose
 * skills carry the engine's one-line description as `line` where the engine reports it.
 * Output: each skill's id, name and one-line description, empty while unread or failed. */
export function catalogOf(skills: Loaded<{ id: string; name: string; description: string; line?: string }[]>): SkillEntry[] {
  return skills.kind === "ok" ? skillEntries(skills.value) : [];
}

/** The Process page for one scope: reads the saved text and the skill catalog from the
 * engine, keeps the owner's draft, and saves only on Save. The seeds read the owner's global
 * file or the default text only on their click, into a preview. Input: where it applies. */
export function ProcessEditor({ scope }: { scope: ProcessScope }) {
  const session = useAppSession();
  const projectId = scope.kind === "project" ? scope.id : null;
  const [saved, reload] = useLoad(() => loadProcess(projectId), [projectId]);
  const [skills] = useLoad(() => listSkills(projectId ?? undefined), [projectId]);
  const [draft, setDraft] = useState("");
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<"save" | "import" | "default" | null>(null);
  const [preview, setPreview] = useState<ProcessPreview | null>(null);
  const [leaving, setLeaving] = useState<SkillEntry | null>(null);
  const loaded = useRef<string | null>(null);
  const savedText = saved.kind === "ok" ? saved.value : null;
  useEffect(() => {
    if (savedText === null) return;
    const before = loaded.current;
    loaded.current = savedText;
    setDraft((current) => keptDraft(current, before, savedText));
  }, [savedText]);
  const open = (skill: SkillEntry) => navigate({ name: "settings", tab: "plugins", skill: skill.id });
  const openSkill = (skill: SkillEntry) => {
    setPreview(null);
    if (savedText !== null && draft !== savedText) setLeaving(skill);
    else open(skill);
  };

  const fail = (result: Result<unknown>) => { if (result.kind !== "ok") setFailure(processMessage(result)); };
  const seed = async (source: "import" | "default") => {
    setFailure("");
    setBusy(source);
    const result = session.noteResult(await (source === "import" ? importGlobalInstructions() : defaultProcess()));
    setBusy(null);
    if (result.kind === "ok") setPreview({ source, text: result.value });
    else fail(result);
  };
  const save = async () => {
    setFailure("");
    setBusy("save");
    const result = session.noteResult(await saveProcess(projectId, draft));
    setBusy(null);
    if (result.kind === "ok") {
      session.showToast(savedMessage(result.value));
      reload();
    } else fail(result);
  };
  const editor = <ProcessView scope={scope} saved={saved} draft={draft} skills={catalogOf(skills)} failure={failure} busy={busy} preview={preview}
    onDraft={setDraft} onSave={() => void save()} onImport={() => void seed("import")} onDefault={() => void seed("default")}
    onUsePreview={() => { if (preview) setDraft(preview.text); setPreview(null); }} onClosePreview={() => setPreview(null)} onRetry={reload}
    leaving={leaving} onOpenSkill={openSkill} onLeave={() => { if (leaving) open(leaving); setLeaving(null); }} onStay={() => setLeaving(null)} />;
  if (scope.kind === "app") return editor;
  return <div className="stack">
    <AgentSettings projectId={scope.id} />
    {editor}
  </div>;
}

/** Settings, Process: the app-wide instructions every agent gets. */
export function ProcessSettings() {
  return <ProcessEditor scope={{ kind: "app" }} />;
}
