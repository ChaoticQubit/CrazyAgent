import { createRef, useRef, useState, type RefObject } from "react";
import {
  addMcpServer, addSkill, approveMcpServer, checkMcpServer, importSkill, listMcpServers, listSkills, pickFolder, plainError, removeMcpSecret, removeMcpServer,
  removeSkill, setMcpSecret, setMcpServerEnabled, setSkillEnabled, skillDetail, skillImportScan, switchPlugin, type ImportCandidate, type ImportedSkill,
  type ImportFrom, type ImportList, type ImportState, type McpServerSpec, type McpServerView, type PluginScope, type Result, type SkillDetail, type SkillView,
} from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import {
  Button, Checkbox, ConfirmDialog, Dialog, EmptyState, LoadError, Menu, NotReady, Notice, Panel, SecretField, Segmented, Skeleton, StatusBadge, Switch, TextArea,
  TextField,
  type MenuItem,
} from "../../ui";
import { navigate } from "../../ui/router";
import { MarkdownView } from "../project/markdown";
import { useLinkOpener } from "../project/links";
import { SkillTag } from "../skill-tag";
import { homePath } from "./harnesses";

const ARGS_MAX = 32;
const SECRETS_MAX = 16;
const NAME_COPY = "Use 1 to 64 lowercase letters, digits, - or _ for the name.";
const SECRET_NAME_COPY = "Use letters, digits, _ or - for the secret name, starting with a letter.";
const COMMAND_COPY = "Enter the full path to the command, starting with /.";
const ARGS_COPY = "Use up to 32 arguments, each on one line.";
const URL_COPY = "Use an https address. Plain http works only for this Mac, and the address can't hold a password.";
const LOOPBACK = new Set(["127.0.0.1", "localhost", "[::1]"]);
const NOT_READY = "This change isn't ready in this build yet.";

const WORDS: Record<McpServerView["status"], string> = {
  approved: "Approved", waiting_for_approval: "Needs approval", off: "Turned off", cant_start: "Can't start",
};

/** The design's status word for an MCP server. Input: the engine status. Output: the word. */
export function serverWord(status: McpServerView["status"]): string {
  return WORDS[status];
}

/** Checks an MCP server name with the engine's rule. Input: the typed name.
 * Output: plain error copy, or undefined when the engine accepts it. */
export function serverNameError(name: string): string | undefined {
  return /^[a-z0-9][a-z0-9_-]{0,63}$/.test(name) ? undefined : NAME_COPY;
}

/** Checks a secret name with the engine's rule (an environment or header name).
 * Input: the typed name. Output: plain error copy, or undefined. */
export function secretNameError(name: string): string | undefined {
  return /^[A-Za-z_][A-Za-z0-9_-]{0,63}$/.test(name) ? undefined : SECRET_NAME_COPY;
}

/** Checks a typed skill folder before the engine checks it again. Input: the typed path.
 * Output: plain error copy, or undefined when it is a full path. */
export function skillFolderError(folder: string): string | undefined {
  const path = folder.trim();
  if (!path) return "Choose a folder that holds SKILL.md.";
  return path.startsWith("/") && !path.includes("\0") ? undefined : "Enter the full path of a folder that holds SKILL.md.";
}

function httpAllowed(text: string): boolean {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return false;
  }
  if (url.username || url.password) return false;
  return url.protocol === "https:" || (url.protocol === "http:" && LOOPBACK.has(url.hostname));
}

export interface ServerForm {
  kind: McpServerSpec["kind"];
  command: string;
  args: string;
  url: string;
}

/** Turns the Add MCP server form into the engine's spec: a full command path with one
 * argument per line, or an https URL (plain http only on this Mac).
 * Input: the typed form. Output: the spec, or the plain error for the field at fault. */
export function serverSpecInput(form: ServerForm): { spec: McpServerSpec } | { error: { command?: string; args?: string; url?: string } } {
  if (form.kind === "http") {
    const url = form.url.trim();
    return httpAllowed(url) ? { spec: { kind: "http", url } } : { error: { url: URL_COPY } };
  }
  const command = form.command.trim();
  if (!command.startsWith("/")) return { error: { command: COMMAND_COPY } };
  const args = form.args.split("\n").map((line) => line.replace(/\r$/, "")).filter((line) => line.trim() !== "");
  if (args.length > ARGS_MAX) return { error: { args: ARGS_COPY } };
  return { spec: { kind: "stdio", command, args } };
}

/** Turns secret rows into name and value pairs; rows with neither are skipped.
 * Input: the rows read once from the masked fields. Output: the pairs, or one plain error. */
export function secretsInput(rows: { name: string; value: string }[]): { secrets: [string, string][] } | { error: string } {
  const filled = rows.filter((row) => row.name.trim() || row.value);
  if (filled.length > SECRETS_MAX) return { error: "A server can have up to 16 secrets." };
  const secrets: [string, string][] = [];
  for (const row of filled) {
    const name = row.name.trim();
    const invalid = secretNameError(name);
    if (invalid) return { error: invalid };
    if (!row.value) return { error: `Enter a value for ${name}.` };
    if (secrets.some(([taken]) => taken === name)) return { error: "Each secret needs its own name." };
    secrets.push([name, row.value]);
  }
  assert(secrets.length <= SECRETS_MAX, "secret rows exceed the engine bound");
  return { secrets };
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The review lines for an MCP server: where it runs or connects, its secrets and where it
 * applies. Input: the server. Output: the lines in display order. */
export function serverFacts(server: McpServerView): string[] {
  const where = server.spec.kind === "stdio" ? `Runs: ${server.runs}` : `Connects to: ${server.spec.url}`;
  const secrets = server.secrets.length
    ? server.secrets.map((secret) => `${secret.name} (${secret.saved ? "Key saved" : "Not saved"})`).join(", ") : "none";
  return [where, `Secrets: ${secrets}`, server.scope_label];
}

/** What a skill or server switch shows. In a project, an item from global settings uses the
 * project's own switch. Inputs: the item and the project id, if any.
 * Output: checked, label, and whether the project switch applies. */
export function pluginSwitch(item: { scope: PluginScope; enabled: boolean; on_here: boolean | null }, projectId?: string): { checked: boolean; label: string; project: boolean } {
  if (projectId && item.scope.kind === "global") return { checked: item.enabled && item.on_here !== false, label: "On in this project", project: true };
  return { checked: item.enabled, label: item.scope.kind === "global" ? "On for every project" : "On", project: false };
}

/** The skills list as a pure view of the engine read. Inputs: the read, the project id when
 * shown in a project, the last failure and the actions. */
export function SkillsView({ skills, projectId, failure, onAdd, onImport, onOpen, onToggle, onRemove, onRetry }: {
  skills: Loaded<SkillView[]>;
  projectId?: string;
  failure: string;
  onAdd: () => void;
  onImport?: () => void;
  onOpen: (skill: SkillView) => void;
  onToggle: (skill: SkillView, on: boolean) => void;
  onRemove: (skill: SkillView) => void;
  onRetry: () => void;
}) {
  if (skills.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (skills.kind === "not_ready") return <NotReady what="Skills" compact />;
  if (skills.kind === "error") return <LoadError message="Couldn't load skills." onRetry={onRetry} />;
  const add = <span className="row">
    {onImport && <Button icon="folder-input" onClick={onImport}>Import my skills</Button>}
    <Button icon="folder-plus" onClick={onAdd}>Add skill</Button>
  </span>;
  return <div className="stack-small">
    <div className="row between">
      <p className="secondary">A skill is a folder with a SKILL.md file. Global skills are available to every project; you can switch them off per project.</p>
      {skills.value.length > 0 && add}
    </div>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {skills.value.length === 0 ? <EmptyState icon="book-open" title="No skills yet" compact action={add} /> : <Panel padded={false}><div className="rows">
      {skills.value.map((skill, index) => {
        const on = pluginSwitch(skill, projectId);
        return <div className="item-row" key={skill.id}>
          <div className="stack-small"><span className="title"><SkillTag name={skill.name} skills={[{ id: skill.id, name: skill.name, description: skill.line || skill.description }]}
            onOpen={() => onOpen(skill)} /></span><span className="secondary">{skill.description}</span>
            <span className="mono muted small">{homePath(skill.source)}</span></div>
          <span className="row">
            <Button variant="quiet" size="small" icon="book-open" aria-label={`Open ${skill.name}`} onClick={() => onOpen(skill)}>Open</Button>
            <Switch id={`skill-on-${index}`} label={on.label} checked={on.checked} onChecked={(next) => onToggle(skill, next)}
              disabledReason={on.project && !skill.enabled ? "Turned off for every project in Settings." : undefined} />
            {!on.project && <Menu label={`More for ${skill.name}`} items={[{ label: "Remove", icon: "trash", danger: true, onSelect: () => onRemove(skill) }]} />}
          </span>
        </div>;
      })}
    </div></Panel>}
  </div>;
}

/** Drops the frontmatter block at the top of a SKILL.md, which the detail view shows as the
 * skill's name and description instead. Input: the file text. Output: the Markdown body. */
export function skillBody(text: string): string {
  const source = text.replace(/^\uFEFF/, "");
  const lines = source.split("\n");
  if (lines[0]?.trimEnd() !== "---") return source;
  const end = lines.findIndex((line, index) => index > 0 && line.trimEnd() === "---");
  return end < 0 ? source : lines.slice(end + 1).join("\n").replace(/^(\s*\n)+/, "");
}

/** A file size as the detail view lists it. Input: bytes. Output: such as "812 B" or "4.2 KB". */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** One skill's detail as a pure view of the engine read: description, where it applies and
 * comes from, its switch, the rendered SKILL.md and its files. Inputs: the read, the project id
 * when shown in a project, the last failure and the actions. */
export function SkillDetailView({ detail, projectId, failure, onToggle, onOpenLink, onRetry }: {
  detail: Loaded<SkillDetail>;
  projectId?: string;
  failure: string;
  onToggle: (skill: SkillView, on: boolean) => void;
  onOpenLink: (href: string) => void;
  onRetry: () => void;
}) {
  if (detail.kind === "loading") return <Skeleton shape="row" count={6} />;
  if (detail.kind === "not_ready") return <NotReady what="Skill details" compact />;
  if (detail.kind === "error") return <LoadError message={detail.message} onRetry={onRetry} />;
  const { skill, text, files, files_more } = detail.value;
  const on = pluginSwitch(skill, projectId);
  return <div className="stack">
    <p className="secondary">{skill.description}</p>
    <div className="row between">
      <span className="stack-small"><span className="secondary">{skill.scope_label}</span><span className="mono muted small">{homePath(skill.source)}</span></span>
      <Switch id="skill-detail-on" label={on.label} checked={on.checked} onChecked={(next) => onToggle(skill, next)}
        disabledReason={on.project && !skill.enabled ? "Turned off for every project in Settings." : undefined} />
    </div>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    <Panel title="SKILL.md"><MarkdownView source={skillBody(text)} onOpenLink={onOpenLink} /></Panel>
    <Panel title={`Files (${files.length + files_more})`} padded={false}><div className="rows">
      {files.map((file) => <div className="item-row" key={file.path}><span className="mono small">{file.path}</span><span className="muted small">{fileSize(file.bytes)}</span></div>)}
      {files_more > 0 && <div className="item-row"><span className="secondary">And {files_more} more</span></div>}
    </div></Panel>
  </div>;
}

/** The skill detail pop-up: reads the skill from the engine and switches it the same way the
 * list does. Inputs: the skill id, the project id when opened in a project, close and changed. */
export function SkillDetailDialog({ id, projectId, onClose, onChanged }: {
  id: string;
  projectId?: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const session = useAppSession();
  const openLink = useLinkOpener(session.showToast);
  const [detail, reload] = useLoad(() => skillDetail(id, projectId), [id, projectId], "skill-detail");
  const [failure, setFailure] = useState("");
  const toggle = async (skill: SkillView, on: boolean) => {
    setFailure("");
    const target = pluginSwitch(skill, projectId);
    const result = session.noteResult<unknown>(target.project && projectId ? await switchPlugin(projectId, skill.id, on) : await setSkillEnabled(skill.id, on));
    if (result.kind === "ok") { session.showToast(on ? "Skill turned on." : "Skill turned off."); reload(); onChanged(); }
    else setFailure(result.kind === "not_ready" ? NOT_READY : result.message);
  };
  const title = detail.kind === "ok" ? detail.value.skill.name : "Skill";
  return <Dialog open title={title} onClose={onClose} wide footer={<Button onClick={onClose}>Close</Button>}>
    <SkillDetailView detail={detail} projectId={projectId} failure={failure} onRetry={reload} onOpenLink={openLink}
      onToggle={(skill, on) => void toggle(skill, on)} />
  </Dialog>;
}

const IMPORT_FROM: Record<ImportFrom, string> = { claude: "Claude Code", codex: "Codex" };

/** Says whether a found skill can be ticked: new, or added with different files.
 * Input: its state. Output: true for "new" and "differs". */
export function importable(state: ImportState): boolean {
  return state.kind === "new" || state.kind === "differs";
}

/** The badge word for a found skill. Input: its state. Output: "" for a new skill,
 * "Already added", "Differs" or "Can't import". */
export function importWord(state: ImportState): string {
  if (state.kind === "added") return "Already added";
  if (state.kind === "differs") return "Differs";
  if (state.kind === "unusable") return "Can't import";
  return "";
}

/** One ticked skill's import outcome as the dialog shows it. */
export interface ImportOutcome {
  name: string;
  result: Result<ImportedSkill>;
}

/** The sentence for one import outcome. Input: the outcome. Output: what was imported and
 * which files were left out, or why it was not imported. */
export function importSentence({ name, result }: ImportOutcome): string {
  if (result.kind === "not_ready") return `Couldn't import ${name}: ${NOT_READY}`;
  if (result.kind !== "ok") return `Couldn't import ${name}: ${result.message}`;
  const { skipped, skipped_total } = result.value;
  if (skipped_total === 0) return `Imported ${name}.`;
  const rest = skipped_total - skipped.length;
  return `Imported ${name}. Left out ${skipped.join(", ")}${rest > 0 ? ` and ${rest} more` : ""}.`;
}

/** The found skills as a pure view of the scan: a tick box for each one that can be imported,
 * its source and state, then the last import's outcomes. Inputs: the scan, the ticked
 * folders, the outcomes and the actions. */
export function ImportSkillsView({ list, ticked, outcomes, onTick, onRetry }: {
  list: Loaded<ImportList>;
  ticked: ReadonlySet<string>;
  outcomes: ImportOutcome[];
  onTick: (found: ImportCandidate, on: boolean) => void;
  onRetry: () => void;
}) {
  if (list.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (list.kind === "not_ready") return <NotReady what="Importing skills" compact />;
  if (list.kind === "error") return <LoadError message={list.message} onRetry={onRetry} />;
  const { found, more } = list.value;
  return <div className="stack-small">
    <p className="secondary">From ~/.claude/skills and ~/.codex/skills. Ticked skills are copied into CrazyAgent and turned on for every project. Dotfiles and files that look like keys or passwords are left out, and your folders don't change.</p>
    {outcomes.map((outcome, index) => <Notice key={`${index}-${outcome.name}`} tone={outcome.result.kind === "ok" ? "ok" : "danger"}>{importSentence(outcome)}</Notice>)}
    {found.length === 0 ? <EmptyState icon="folder-input" title="No skills found in those folders" compact /> : <Panel padded={false}><div className="rows">
      {found.map((item) => {
        const word = importWord(item.state);
        const why = item.state.kind === "unusable" ? plainError(item.state.code)
          : item.state.kind === "differs" ? "Importing replaces the added copy and keeps its project switches." : "";
        return <div className="item-row" key={item.folder}>
          <Checkbox checked={ticked.has(item.folder)} disabled={!importable(item.state)} onChecked={(on) => onTick(item, on)}
            label={<span className="stack-small"><span className="title">{item.name}</span>{item.line && <span className="secondary">{item.line}</span>}
              <span className="mono muted small">{IMPORT_FROM[item.from]}: {homePath(item.folder)}</span>{why && <span className="secondary">{why}</span>}</span>} />
          {word && <StatusBadge word={word} tone={item.state.kind === "unusable" ? "danger" : item.state.kind === "differs" ? "warn" : "neutral"} />}
        </div>;
      })}
      {more > 0 && <div className="item-row"><span className="secondary">And {more} more not shown</span></div>}
    </div></Panel>}
  </div>;
}

/** The Import my skills pop-up. It reads the owner's skills folders only once it opens, and
 * imports the ticked skills one at a time. Inputs: close and a callback after any import. */
export function ImportSkillsDialog({ onClose, onChanged }: { onClose: () => void; onChanged: () => void }) {
  const session = useAppSession();
  const [list, reload] = useLoad(() => skillImportScan(), []);
  const [ticked, setTicked] = useState<ReadonlySet<string>>(new Set());
  const [outcomes, setOutcomes] = useState<ImportOutcome[]>([]);
  const [pending, setPending] = useState(false);
  const picks = list.kind === "ok" ? list.value.found.filter((item) => ticked.has(item.folder) && importable(item.state)) : [];
  const tick = (item: ImportCandidate, on: boolean) => {
    const next = new Set(ticked);
    if (on) next.add(item.folder); else next.delete(item.folder);
    setTicked(next);
  };
  const run = async () => {
    setPending(true);
    const done: ImportOutcome[] = [];
    for (const item of picks) {
      const replace = item.state.kind === "differs" ? item.state.id : null;
      done.push({ name: item.name, result: session.noteResult(await importSkill(item.folder, replace)) });
    }
    setPending(false);
    setOutcomes(done);
    setTicked(new Set());
    const imported = done.filter((outcome) => outcome.result.kind === "ok").length;
    if (imported > 0) { session.showToast(imported === 1 ? "Imported 1 skill." : `Imported ${imported} skills.`); onChanged(); }
    reload();
  };
  return <Dialog open title="Import my skills" onClose={onClose} wide
    footer={<><Button onClick={onClose}>Close</Button><Button variant="primary" disabled={picks.length === 0} pending={pending} pendingLabel="Importing..."
      onClick={() => void run()}>{picks.length > 0 ? `Import ${picks.length}` : "Import"}</Button></>}>
    <ImportSkillsView list={list} ticked={ticked} outcomes={outcomes} onTick={tick} onRetry={reload} />
  </Dialog>;
}

/** The MCP server list as a pure view of the engine read, with the approval review for
 * servers that need it. Inputs: the read, project id, last failure, the server being
 * checked and the actions. */
export function ServersView({ servers, projectId, failure, busy, onAdd, onToggle, onApprove, onCheck, onSecrets, onRemove, onRetry }: {
  servers: Loaded<McpServerView[]>;
  projectId?: string;
  failure: string;
  busy: string | null;
  onAdd: () => void;
  onToggle: (server: McpServerView, on: boolean) => void;
  onApprove: (server: McpServerView) => void;
  onCheck: (server: McpServerView) => void;
  onSecrets: (server: McpServerView) => void;
  onRemove: (server: McpServerView) => void;
  onRetry: () => void;
}) {
  if (servers.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (servers.kind === "not_ready") return <NotReady what="MCP servers" compact />;
  if (servers.kind === "error") return <LoadError message="Couldn't load MCP servers." onRetry={onRetry} />;
  const add = <Button icon="plus" onClick={onAdd}>Add MCP server</Button>;
  return <div className="stack-small">
    <div className="row between">
      <p className="secondary">MCP servers give agents extra tools. The engine checks each server, and you approve it before agents can use it.</p>
      {servers.value.length > 0 && add}
    </div>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {servers.value.length === 0 ? <EmptyState icon="plug" title="No MCP servers yet" compact action={add} /> : <Panel padded={false}><div className="rows">
      {servers.value.map((server, index) => {
        const on = pluginSwitch(server, projectId);
        const command = server.spec.kind === "stdio";
        const menu: MenuItem[] = [
          ...(command ? [{ label: "Check", icon: "refresh-cw" as const, onSelect: () => onCheck(server) }] : []),
          ...(on.project ? [] : [{ label: "Secrets", icon: "key-round" as const, onSelect: () => onSecrets(server) },
            { label: "Remove", icon: "trash" as const, danger: true, onSelect: () => onRemove(server) }]),
        ];
        return <div key={server.id}>
          <div className="item-row">
            <div className="stack-small">
              <span className="row"><span className="title">{server.name}</span><StatusBadge word={serverWord(server.status)} /></span>
              <span className="mono muted small">{server.runs}</span>
              {server.status === "cant_start" && <span className="row">
                <span className="field-error">Can't start: {server.problem ?? "the server didn't answer."}</span>
                {command && <Button size="small" icon="refresh-cw" pending={busy === server.id} pendingLabel="Checking..." onClick={() => onCheck(server)}>Check again</Button>}
              </span>}
              {server.tools.length > 0 && <span className="secondary">Tools: {server.tools.join(", ")}</span>}
            </div>
            <span className="row">
              <Switch id={`server-on-${index}`} label={on.label} checked={on.checked} onChecked={(next) => onToggle(server, next)}
                disabledReason={on.project && !server.enabled ? "Turned off for every project in Settings." : undefined} />
              {menu.length > 0 && <Menu label={`More for ${server.name}`} items={menu} />}
            </span>
          </div>
          {server.status === "waiting_for_approval" && <div className="review">
            {command ? <span>Runs: <span className="mono">{server.runs}</span></span>
              : <span>Connects to: <span className="mono">{server.spec.kind === "http" ? server.spec.url : ""}</span></span>}
            {serverFacts(server).slice(1).map((fact) => <span key={fact}>{fact}</span>)}
            <span className="row">
              <Button variant="primary" icon="shield-check" pending={busy === server.id} pendingLabel="Approving..." onClick={() => onApprove(server)}>Approve</Button>
              {!on.project && <Button icon="trash" onClick={() => onRemove(server)}>Remove</Button>}
            </span>
          </div>}
        </div>;
      })}
    </div></Panel>}
  </div>;
}

/** The Add skill dialog: the macOS folder chooser, or a typed full path when the chooser is
 * unavailable. Inputs: open, where the skill applies, a starting message, close and added. */
export function AddSkillDialog({ open, scope, message, onClose, onAdded }: {
  open: boolean;
  scope: PluginScope;
  message?: string;
  onClose: () => void;
  onAdded: (skill: SkillView) => void;
}) {
  const session = useAppSession();
  const [folder, setFolder] = useState("");
  const [error, setError] = useState<string>();
  const [form, setForm] = useState<string>();
  const [pending, setPending] = useState(false);
  const [picking, setPicking] = useState(false);
  const choose = async () => {
    setPicking(true);
    setForm(undefined);
    const picked = await pickFolder();
    setPicking(false);
    if (picked.kind === "ok") {
      if (picked.value) { setFolder(picked.value); setError(undefined); }
    } else setForm(picked.kind === "not_ready" ? "The folder chooser isn't ready in this build. Type the full path instead." : picked.message);
  };
  const save = async () => {
    const invalid = skillFolderError(folder);
    setError(invalid);
    if (invalid) return;
    setPending(true);
    const added = session.noteResult(await addSkill(scope, folder.trim()));
    setPending(false);
    if (added.kind === "ok") { setFolder(""); setForm(undefined); onAdded(added.value); }
    else setForm(added.kind === "not_ready" ? NOT_READY : added.message);
  };
  const shown = form ?? message;
  return <Dialog open={open} title="Add skill" onClose={onClose}
    footer={<><Button onClick={onClose}>Cancel</Button><Button variant="primary" type="submit" form="add-skill-form" pending={pending} pendingLabel="Adding...">Add skill</Button></>}>
    <form id="add-skill-form" className="stack" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <p className="secondary">Pick a folder with a SKILL.md file. CrazyAgent keeps its own copy and never runs anything in it.</p>
      <div className="row"><Button icon="folder-open" pending={picking} pendingLabel="Choosing..." onClick={() => void choose()}>Choose folder</Button></div>
      <TextField id="skill-folder" label="Folder" value={folder} onValue={setFolder} error={error} spellCheck={false} autoComplete="off"
        helper="Or type its full path, starting with /." />
      {shown && <Notice tone="danger" alert>{shown}</Notice>}
    </form>
  </Dialog>;
}

interface SecretRow {
  name: string;
  value: RefObject<HTMLInputElement | null>;
}

function readSecrets(rows: SecretRow[]): { name: string; value: string }[] {
  return rows.map((row) => ({ name: row.name, value: row.value.current?.value ?? "" }));
}

function clearSecrets(rows: SecretRow[]): void {
  for (const row of rows) if (row.value.current) row.value.current.value = "";
}

/** The Add MCP server dialog: name, a command on this Mac or a URL, and secrets by name.
 * Secret values stay in masked fields until submit, are sent once and then cleared.
 * Inputs: open, where the server applies, close and added handlers. */
export function AddServerDialog({ open, scope, onClose, onAdded }: {
  open: boolean;
  scope: PluginScope;
  onClose: () => void;
  onAdded: (server: McpServerView) => void;
}) {
  const session = useAppSession();
  const [name, setName] = useState("");
  const [form, setForm] = useState<ServerForm>({ kind: "stdio", command: "", args: "", url: "" });
  const [rows, setRows] = useState<SecretRow[]>([]);
  const [errors, setErrors] = useState<{ name?: string; command?: string; args?: string; url?: string; secrets?: string; form?: string }>({});
  const [pending, setPending] = useState(false);
  const save = async () => {
    const spec = serverSpecInput(form);
    const next = { name: serverNameError(name.trim()), ...("error" in spec ? spec.error : {}) };
    setErrors(next);
    if (next.name || "error" in spec) return;
    const secrets = secretsInput(readSecrets(rows));
    if ("error" in secrets) return setErrors({ secrets: secrets.error });
    clearSecrets(rows);
    setPending(true);
    try {
      const added = session.noteResult(await addMcpServer(scope, name.trim(), spec.spec, secrets.secrets));
      if (added.kind === "ok") {
        setName("");
        setForm({ kind: "stdio", command: "", args: "", url: "" });
        setRows([]);
        onAdded(added.value);
      } else setErrors({ form: added.kind === "not_ready" ? NOT_READY : `${added.message} Enter the secret values again.` });
    } finally {
      setPending(false);
    }
  };
  const close = () => { clearSecrets(rows); onClose(); };
  return <Dialog open={open} title="Add MCP server" onClose={close} wide
    footer={<><Button onClick={close}>Cancel</Button><Button variant="primary" type="submit" form="add-server-form" pending={pending} pendingLabel="Adding...">Add server</Button></>}>
    <form id="add-server-form" className="stack" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <TextField id="server-name" label="Name" value={name} onValue={setName} error={errors.name} maxLength={64} spellCheck={false} autoComplete="off"
        helper="Lowercase letters, digits, - or _, such as github." />
      <Segmented label="Type" value={form.kind} onValue={(kind) => setForm({ ...form, kind })}
        options={[{ value: "stdio", label: "Command on this Mac", icon: "terminal" }, { value: "http", label: "URL", icon: "link" }]} />
      {form.kind === "stdio" ? <>
        <TextField id="server-command" label="Command" value={form.command} onValue={(command) => setForm({ ...form, command })} error={errors.command}
          spellCheck={false} autoComplete="off" helper="The full path, such as /opt/homebrew/bin/npx." />
        <TextArea id="server-args" label="Arguments" value={form.args} onValue={(args) => setForm({ ...form, args })} error={errors.args} rows={3}
          helper="One per line." />
      </> : <TextField id="server-url" label="URL" type="url" value={form.url} onValue={(url) => setForm({ ...form, url })} error={errors.url}
        spellCheck={false} autoComplete="off" helper="An https address, or http://127.0.0.1 for a server on this Mac." />}
      <div className="stack-small">
        <span className="title">Secrets</span>
        <p className="secondary">{form.kind === "stdio" ? "Each secret is given to the command as an environment value." : "Each secret is sent to the URL as a header."} CrazyAgent's engine keeps them privately; you won't see them again.</p>
        {rows.map((row, index) => <div className="row" key={index}>
          <TextField id={`server-secret-name-${index}`} label="Secret name" value={row.name} spellCheck={false} autoComplete="off"
            onValue={(value) => setRows(rows.map((other, at) => at === index ? { ...other, name: value } : other))} />
          <SecretField id={`server-secret-value-${index}`} label="Value" inputRef={row.value} />
          <Button iconOnly="Remove secret" icon="trash" onClick={() => { if (row.value.current) row.value.current.value = ""; setRows(rows.filter((_, at) => at !== index)); }} />
        </div>)}
        {rows.length < SECRETS_MAX && <span><Button size="small" icon="plus" onClick={() => setRows([...rows, { name: "", value: createRef<HTMLInputElement>() }])}>Add secret</Button></span>}
        {errors.secrets && <span className="field-error" role="alert">{errors.secrets}</span>}
      </div>
      {errors.form && <Notice tone="danger" alert>{errors.form}</Notice>}
      <p className="secondary">The server waits for your approval before agents can use it.</p>
    </form>
  </Dialog>;
}

/** The secrets of one MCP server: names with saved state, replace or add one value, delete
 * one. The value is sent once and cleared. Inputs: the server, close and changed handlers. */
export function SecretsDialog({ server, onClose, onChanged }: {
  server: McpServerView;
  onClose: () => void;
  onChanged: (server: McpServerView) => void;
}) {
  const session = useAppSession();
  const value = useRef<HTMLInputElement>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState<string | null>(null);
  const finish = (result: Result<McpServerView>, done: string) => {
    const noted = session.noteResult(result);
    if (noted.kind === "ok") { session.showToast(done); onChanged(noted.value); }
    else setError(noted.kind === "not_ready" ? NOT_READY : noted.message);
  };
  const save = async () => {
    const invalid = secretNameError(name.trim()) ?? (value.current?.value ? undefined : `Enter a value for ${name.trim()}.`);
    setError(invalid);
    if (invalid) return;
    let secret = value.current?.value ?? "";
    if (value.current) value.current.value = "";
    setPending("save");
    try {
      const sending = setMcpSecret(server.id, name.trim(), secret);
      secret = "";
      finish(await sending, "Secret saved.");
      setName("");
    } finally {
      secret = "";
      setPending(null);
    }
  };
  const remove = async (secret: string) => {
    setError(undefined);
    setPending(secret);
    finish(await removeMcpSecret(server.id, secret), "Secret deleted.");
    setPending(null);
  };
  const close = () => { if (value.current) value.current.value = ""; onClose(); };
  return <Dialog open title={`Secrets for ${server.name}`} onClose={close} footer={<Button onClick={close}>Done</Button>}>
    <div className="stack">
      {server.secrets.length === 0 ? <p className="secondary">No secrets yet.</p> : <div className="rows">
        {server.secrets.map((secret) => <div className="item-row" key={secret.name}>
          <span className="row"><span className="mono">{secret.name}</span><StatusBadge word={secret.saved ? "Key saved" : "Not saved"} /></span>
          <Button variant="danger-text" size="small" icon="trash" pending={pending === secret.name} pendingLabel="Deleting..." onClick={() => void remove(secret.name)}>Delete</Button>
        </div>)}
      </div>}
      <form className="stack-small" onSubmit={(event) => { event.preventDefault(); void save(); }}>
        <TextField id="secret-name" label="Secret name" value={name} onValue={setName} spellCheck={false} autoComplete="off"
          helper="Use an existing name to replace its value." />
        <SecretField id="secret-value" label="Value" inputRef={value} helper="Sent once to CrazyAgent's engine. You won't see it again." />
        {error && <Notice tone="danger" alert>{error}</Notice>}
        <span><Button type="submit" variant="primary" icon="key-round" pending={pending === "save"} pendingLabel="Saving...">Save secret</Button></span>
      </form>
    </div>
  </Dialog>;
}

type Removing = { kind: "skill"; skill: SkillView } | { kind: "server"; server: McpServerView };

/** Settings, Plugins and skills: skills and MCP servers read from the engine, with add,
 * switch, remove, approve, check, secrets and each skill's detail. Global by default; with a
 * project id it shows that project's view and adds items to that project only. Inputs: optional
 * project id and, in global settings, the skill id from the route whose detail is open. */
export function PluginsAndSkills({ projectId, skill }: { projectId?: string; skill?: string } = {}) {
  const session = useAppSession();
  const scope: PluginScope = projectId ? { kind: "project", project_id: projectId } : { kind: "global" };
  const [section, setSection] = useState<"skills" | "servers">("skills");
  const [skills, reloadSkills] = useLoad(() => listSkills(projectId), [projectId], "skills");
  const [servers, reloadServers] = useLoad(() => listMcpServers(projectId), [projectId], "mcp-servers");
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [adding, setAdding] = useState<{ kind: "skill"; message?: string } | { kind: "server" }>();
  const [secrets, setSecrets] = useState<McpServerView>();
  const [removing, setRemoving] = useState<Removing>();
  const [opened, setOpened] = useState<string>();
  const [importing, setImporting] = useState(false);
  const shown = projectId ? opened : skill;
  const open = (id: string) => projectId ? setOpened(id) : navigate({ name: "settings", tab: "plugins", skill: id });
  const closeDetail = () => projectId ? setOpened(undefined) : navigate({ name: "settings", tab: "plugins" }, true);

  async function act<T>(id: string, action: () => Promise<Result<T>>, done: (value: T) => string, reload: () => void): Promise<void> {
    setFailure("");
    setBusy(id);
    const result = session.noteResult(await action());
    setBusy(null);
    if (result.kind === "ok") { session.showToast(done(result.value)); reload(); }
    else { setFailure(result.kind === "not_ready" ? NOT_READY : result.message); if (result.kind === "error" && /changed|not_found/.test(result.code)) reload(); }
  }

  const addSkillFromChooser = async () => {
    setFailure("");
    const picked = await pickFolder();
    if (picked.kind !== "ok") return setAdding({ kind: "skill", message: picked.kind === "not_ready" ? "The folder chooser isn't ready in this build. Type the full path instead." : picked.message });
    const folder = picked.value;
    if (folder === null) return;
    await act(folder, () => addSkill(scope, folder), (skill) => `Added ${skill.name}.`, reloadSkills);
  };

  const toggleSkill = (skill: SkillView, on: boolean) => {
    const target = pluginSwitch(skill, projectId);
    void act<unknown>(skill.id, () => target.project && projectId ? switchPlugin(projectId, skill.id, on) : setSkillEnabled(skill.id, on),
      () => on ? "Skill turned on." : "Skill turned off.", reloadSkills);
  };
  const toggleServer = (server: McpServerView, on: boolean) => {
    const target = pluginSwitch(server, projectId);
    void act<unknown>(server.id, () => target.project && projectId ? switchPlugin(projectId, server.id, on) : setMcpServerEnabled(server.id, on),
      () => on ? "Server turned on." : "Server turned off.", reloadServers);
  };
  const confirmRemove = () => {
    if (!removing) return;
    const item = removing;
    setRemoving(undefined);
    if (item.kind === "skill") void act(item.skill.id, () => removeSkill(item.skill.id), () => "Skill removed.", reloadSkills);
    else void act(item.server.id, () => removeMcpServer(item.server.id), () => "Server removed.", reloadServers);
  };

  return <div className="stack">
    <Segmented label="Plugins and skills" value={section} onValue={(next) => { setFailure(""); setSection(next); }}
      options={[{ value: "skills", label: "Skills", icon: "book-open" }, { value: "servers", label: "MCP servers", icon: "plug" }]} />
    {section === "skills"
      ? <SkillsView skills={skills} projectId={projectId} failure={failure} onRetry={reloadSkills} onAdd={() => void addSkillFromChooser()}
        onImport={projectId ? undefined : () => setImporting(true)} onOpen={(item) => open(item.id)} onToggle={toggleSkill} onRemove={(skill) => setRemoving({ kind: "skill", skill })} />
      : <ServersView servers={servers} projectId={projectId} failure={failure} busy={busy} onRetry={reloadServers} onAdd={() => setAdding({ kind: "server" })}
        onToggle={toggleServer} onSecrets={setSecrets} onRemove={(server) => setRemoving({ kind: "server", server })}
        onApprove={(server) => void act(server.id, () => approveMcpServer(server.id, server.spec), () => "Server approved.", reloadServers)}
        onCheck={(server) => void act(server.id, () => checkMcpServer(server.id),
          (checked) => checked.status === "cant_start" ? `${checked.name} can't start.` : `${checked.name} answered with ${checked.tools.length} ${checked.tools.length === 1 ? "tool" : "tools"}.`,
          reloadServers)} />}
    <AddSkillDialog open={adding?.kind === "skill"} scope={scope} message={adding?.kind === "skill" ? adding.message : undefined} onClose={() => setAdding(undefined)}
      onAdded={(skill) => { setAdding(undefined); session.showToast(`Added ${skill.name}.`); reloadSkills(); }} />
    <AddServerDialog open={adding?.kind === "server"} scope={scope} onClose={() => setAdding(undefined)}
      onAdded={(server) => { setAdding(undefined); session.showToast(`Added ${server.name}. Review it, then approve it.`); reloadServers(); }} />
    {shown && <SkillDetailDialog id={shown} projectId={projectId} onClose={closeDetail} onChanged={reloadSkills} />}
    {importing && <ImportSkillsDialog onClose={() => setImporting(false)} onChanged={reloadSkills} />}
    {secrets && <SecretsDialog server={secrets} onClose={() => setSecrets(undefined)} onChanged={(server) => { setSecrets(server); reloadServers(); }} />}
    <ConfirmDialog open={Boolean(removing)} title={removing?.kind === "server" ? "Remove this MCP server?" : "Remove this skill?"} confirmLabel="Remove"
      onCancel={() => setRemoving(undefined)} onConfirm={confirmRemove}>
      {removing?.kind === "server" ? "Agents stop using it, and its saved secrets are deleted."
        : `Agents stop using it. CrazyAgent deletes its copy; your folder${removing ? ` at ${homePath(removing.skill.source)}` : ""} stays.`}
    </ConfirmDialog>
  </div>;
}
