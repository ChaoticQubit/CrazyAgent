import { useEffect, useState } from "react";
import { listSkills, type Result } from "../api";
import {
  PROCESS_TEXT_BYTES_MAX, defaultProcess, importGlobalInstructions, loadProcess, processMessage, processTextError, saveProcess,
  skillSegments, textBytes, type SavedText, type SkillEntry,
} from "../api-process";
import { useAppSession, useLoad, type Loaded } from "../state";
import { Button, Dialog, LoadError, NotReady, Notice, Panel, Skeleton, TextArea } from "../ui";
import { ConflictView, keepMine, type Conflict } from "./conflict";
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
 * description and clicking opens that skill in Settings. Inputs: the text and the catalog. */
export function TaggedText({ text, skills }: { text: string; skills: readonly SkillEntry[] }) {
  return <div className="process-text">
    {skillSegments(text, skills).map((segment, index) => segment.kind === "text"
      ? <span key={index}>{segment.text}</span>
      : <SkillTag key={index} name={segment.name} skills={[segment]} />)}
  </div>;
}

/** Writes a byte count the way the Process pages show sizes. Input: bytes. Output: the words. */
export function byteSize(bytes: number): string {
  return bytes < 1024 ? `${bytes} bytes` : `${(bytes / 1024).toFixed(1)} KB`;
}

/** The conflict a stale instructions save shows: the version the save named, the text that
 * landed with its version, and who saved it. Input: the stale reply. Output: the conflict. */
export function conflictOf(stale: Extract<SavedText, { kind: "stale" }>): Conflict<string> {
  return { opened: stale.version, version: stale.current.version, current: stale.current.text, writer: stale.writer };
}

/** The Process page as a pure view: the editor with Save, the text with skill tags, and at
 * app level the two seeds, each opening a preview first. After a stale save it shows the
 * conflict, whose Reload and Keep mine are then the only ways on. Inputs: where it applies,
 * the saved text, the draft, the skill catalog, the last failure, what is running, the
 * preview, the conflict and the actions. Nothing reads the owner's files or saves without
 * the matching click. */
export function ProcessView({ scope, saved, draft, skills, failure, busy, preview, conflict, onDraft, onSave, onImport, onDefault, onUsePreview, onClosePreview, onRetry, onReload, onKeepMine }: {
  scope: ProcessScope;
  saved: Loaded<string>;
  draft: string;
  skills: readonly SkillEntry[];
  failure: string;
  busy: "save" | "import" | "default" | null;
  preview: ProcessPreview | null;
  conflict: Conflict<string> | null;
  onDraft: (text: string) => void;
  onSave: () => void;
  onImport: () => void;
  onDefault: () => void;
  onUsePreview: () => void;
  onClosePreview: () => void;
  onRetry: () => void;
  onReload: () => void;
  onKeepMine: () => void;
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
    {conflict && <ConflictView conflict={conflict} busy={busy === "save"} onReload={onReload} onKeepMine={onKeepMine}>
      <TaggedText text={conflict.current} skills={skills} />
    </ConflictView>}
    {seeds}
    <Panel title="Instructions">
      <form className="stack-small" onSubmit={(event) => { event.preventDefault(); if (!invalid && changed && !conflict) onSave(); }}>
        <TextArea id={`process-${scope.kind}`} label="Instructions for agents" value={draft} onValue={onDraft} rows={16} error={invalid}
          helper={`${byteSize(textBytes(draft))} of ${byteSize(PROCESS_TEXT_BYTES_MAX)}${changed ? ". Not saved yet." : ""}`} />
        <span className="row">
          <Button type="submit" variant="primary" icon="check" pending={busy === "save"} pendingLabel="Saving..."
            disabled={Boolean(invalid) || !changed || busy !== null || Boolean(conflict)}>Save</Button>
          {changed && !conflict && <Button disabled={busy !== null} onClick={() => onDraft(saved.value)}>Discard edits</Button>}
        </span>
      </form>
    </Panel>
    {draft.trim() && <Panel title="As agents read it"><TaggedText text={draft} skills={skills} /></Panel>}
    <Dialog open={preview !== null} wide title={preview?.source === "import" ? "Your global instructions" : "CrazyAgent default process"}
      onClose={onClosePreview}
      footer={<>
        <Button onClick={onClosePreview}>Cancel</Button>
        <Button variant="primary" icon="check" onClick={onUsePreview}>Put in the editor</Button>
      </>}>
      <p className="secondary">{preview?.source === "import"
        ? "Read from ~/.claude/CLAUDE.md. It replaces the editor's text; nothing is saved until you click Save."
        : "It replaces the editor's text; nothing is saved until you click Save."}</p>
      {preview && <TaggedText text={preview.text} skills={skills} />}
    </Dialog>
  </div>;
}

/** The skill catalog the tags name, from the engine's skill list. Input: the read, whose
 * skills carry the engine's one-line description as `line` where the engine reports it.
 * Output: each skill's id, name and one-line description, empty while unread or failed. */
export function catalogOf(skills: Loaded<{ id: string; name: string; description: string; line?: string }[]>): SkillEntry[] {
  return skills.kind === "ok" ? skillEntries(skills.value) : [];
}

/** The Process page for one scope: reads the saved text and the skill catalog from the
 * engine, keeps the owner's draft, and saves only on Save. A stale save shows the conflict
 * and keeps the draft, which no later read replaces until the owner picks Reload or Keep
 * mine. The seeds read the owner's global file or the default text only on their click,
 * into a preview. Input: where it applies. */
export function ProcessEditor({ scope }: { scope: ProcessScope }) {
  const session = useAppSession();
  const projectId = scope.kind === "project" ? scope.id : null;
  const [loaded, reload] = useLoad(() => loadProcess(projectId), [projectId]);
  const saved: Loaded<string> = loaded.kind === "ok" ? { kind: "ok", value: loaded.value.text } : loaded;
  const [skills] = useLoad(() => listSkills(projectId ?? undefined), [projectId]);
  const [draft, setDraft] = useState("");
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<"save" | "import" | "default" | null>(null);
  const [preview, setPreview] = useState<ProcessPreview | null>(null);
  const [conflict, setConflict] = useState<Conflict<string> | null>(null);
  const savedText = saved.kind === "ok" ? saved.value : null;
  useEffect(() => { if (savedText !== null && conflict === null) setDraft(savedText); }, [savedText]);

  const fail = (result: Result<unknown>) => { if (result.kind !== "ok") setFailure(processMessage(result)); };
  const seed = async (source: "import" | "default") => {
    setFailure("");
    setBusy(source);
    const result = session.noteResult(await (source === "import" ? importGlobalInstructions() : defaultProcess()));
    setBusy(null);
    if (result.kind === "ok") setPreview({ source, text: result.value });
    else fail(result);
  };
  const save = async (shown: Conflict<string> | null) => {
    if (loaded.kind !== "ok") return;
    setFailure("");
    setBusy("save");
    const named = shown ? keepMine(shown) : { version: loaded.value.version, opened: null };
    const result = session.noteResult(await saveProcess(projectId, draft, named.version, named.opened));
    setBusy(null);
    if (result.kind !== "ok") fail(result);
    else if (result.value.kind === "stale") setConflict(conflictOf(result.value));
    else {
      setConflict(null);
      session.showToast("Instructions saved. Agents started from now on get them.");
      reload();
    }
  };
  const reloadConflict = () => {
    if (conflict) setDraft(conflict.current);
    setConflict(null);
    setFailure("");
    reload();
  };
  return <ProcessView scope={scope} saved={saved} draft={draft} skills={catalogOf(skills)} failure={failure} busy={busy} preview={preview} conflict={conflict}
    onDraft={setDraft} onSave={() => void save(null)} onImport={() => void seed("import")} onDefault={() => void seed("default")}
    onUsePreview={() => { if (preview) setDraft(preview.text); setPreview(null); }} onClosePreview={() => setPreview(null)} onRetry={reload}
    onReload={reloadConflict} onKeepMine={() => void save(conflict)} />;
}

/** Settings, Process: the app-wide instructions every agent gets. */
export function ProcessSettings() {
  return <ProcessEditor scope={{ kind: "app" }} />;
}
