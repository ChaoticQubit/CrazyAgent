import { createRef, Fragment, useRef, useState, type RefObject } from "react";
import {
  addHooks, addMcpServer, addSkill, approveMcpServer, checkMcpServer, importSkill, listHooks, listMcpServers, listSkills, pickFolder, plainError, removeMcpSecret,
  removeMcpServer, removeSkill, setHookEnabled, setMcpSecret, setMcpServerEnabled, setSkillEnabled, skillDetail, skillImportScan, switchPlugin, type HookView,
  type ImportCandidate, type ImportedSkill, type ImportFrom, type ImportList, type ImportState, type McpServerSpec, type McpServerView, type PluginScope,
  type Result, type SkillDetail, type SkillView, type Failure,
} from "../../api";
import { BARE_NO_HOOKS, CODEX_HOOK_EVENTS_ONLY, RCLI_NO_HOOKS } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import {
  Button, Checkbox, ConfirmDialog, Dialog, EmptyState, LoadError, NotReady, Notice, Panel, SecretField, Segmented, SettingLine, SettingSection, Skeleton, StatusBadge, Switch, TextArea,
  TextField,
} from "../../ui";
import { navigate } from "../../ui/router";
import { SkillTag } from "../skill-tag";
import { displayText } from "../conversation";
import { homePath } from "./harnesses";
import { ToolSwitches, ToolsSection } from "./tools";

const ARGS_MAX = 32;
const SECRETS_MAX = 16;
const NAME_COPY = "Use 1 to 64 lowercase letters, digits, - or _ for the name.";
const SECRET_NAME_COPY = "Use letters, digits, _ or - for the secret name, starting with a letter.";
const COMMAND_COPY = "Enter the full path to the command, starting with /.";
const ARGS_COPY = "Use up to 32 arguments, each on one line, with at most 4 KB for the command and its arguments together.";
const URL_COPY = "Use an https address. Plain http works only for this Mac, and the address can't hold a password.";
const LOOPBACK = new Set(["127.0.0.1", "localhost", "[::1]"]);
const NOT_READY = "This change isn't ready in this build yet.";

/** Whether a refused skill or server change means what the screen shows is out of date, so it
 * is read again: the item changed since it was shown, went away, or moved on to a newer
 * version. Input: the refusal. Output: true to read again. */
export function readsAgain(failure: Failure): boolean {
  return failure.kind === "error" && /changed|not_found|stale_revision/.test(failure.code);
}

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

/** Whether a project has switched off an item from global settings. The engine sends on_here
 * as true or false for every global item read with a project, and off is the only change a
 * project can make. Inputs: the item and the project id, if any. Output: the answer, always
 * false outside a project. */
export function pluginChanged(item: { scope: PluginScope; on_here: boolean | null }, projectId?: string): boolean {
  return Boolean(projectId) && item.scope.kind === "global" && item.on_here === false;
}

/** Says that agents do not get a stored skill the engine refuses now, and why.
 * Input: the refusal code. Output: the sentence. */
export function skillUnused(code: string): string {
  return `Agents don't get this skill. ${plainError(code)}`;
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
  return <SettingSection title="Skills" actions={skills.value.length > 0 ? add : undefined}
    about="A skill is a folder with a SKILL.md file. Global skills are available to every project; you can switch them off per project.">
    {failure && <div className="set-body"><Notice tone="danger" alert>{failure}</Notice></div>}
    {skills.value.length === 0 ? <div className="set-body"><EmptyState icon="book-open" title="No skills yet" compact action={add} /></div> : skills.value.map((skill, index) => {
      const on = pluginSwitch(skill, projectId);
      return <SettingLine key={skill.id} label={skill.name} changed={pluginChanged(skill, projectId)}
        shown={<SkillTag name={skill.name} skills={[{ id: skill.id, name: skill.name, description: skill.line || skill.description }]} onOpen={() => onOpen(skill)} />}
        help={<><span>{skill.description}</span><span className="small">{skill.origin}</span><span className="mono small">{homePath(skill.source)}</span>
          {skill.refused && <span className="row"><StatusBadge word="Not given to agents" tone="danger" /><span>{skillUnused(skill.refused)}</span></span>}</>}>
        <Switch id={`skill-on-${index}`} label={on.label} checked={on.checked} onChecked={(next) => onToggle(skill, next)}
          disabledReason={on.project && !skill.enabled ? "Turned off for every project in Settings." : undefined} />
        <Button aria-label={`Open ${skill.name}`} onClick={() => onOpen(skill)}>Open</Button>
        {!on.project && <Button variant="quiet" aria-label={`Remove ${skill.name}`} onClick={() => onRemove(skill)}>Remove</Button>}
      </SettingLine>;
    })}
  </SettingSection>;
}

/** Drops the frontmatter block at the top of a SKILL.md: the detail view shows it as the
 * skill's name and description, plus any other header line agents read. Input: the file text.
 * Output: the Markdown body. */
export function skillBody(text: string): string {
  const source = text.replace(/^\uFEFF/, "");
  const lines = source.split("\n");
  if (lines[0]?.trimEnd() !== "---") return source;
  const end = lines.findIndex((line, index) => index > 0 && line.trimEnd() === "---");
  return end < 0 ? source : lines.slice(end + 1).join("\n").replace(/^(\s*\n)+/, "");
}

/** The SKILL.md header lines agents read that the detail view does not already show as the
 * skill's name and description: every non-blank line of a closed frontmatter block other
 * than the plain `name:` and `description:` lines. Inputs: the file text and the skill.
 * Output: the lines in file order, none when the file has no closed header. */
export function skillHeaderExtra(text: string, skill: { name: string; description: string }): string[] {
  const lines = text.replace(/^\uFEFF/, "").split("\n").map((line) => line.replace(/\r$/, ""));
  if (lines[0]?.trimEnd() !== "---") return [];
  const end = lines.findIndex((line, index) => index > 0 && line.trimEnd() === "---");
  if (end < 0) return [];
  return lines.slice(1, end)
    .filter((line) => line.trim() !== "" && line !== `name: ${skill.name}` && line !== `description: ${skill.description}`);
}

/** A file size as the detail view lists it. Input: bytes. Output: such as "812 B" or "4.2 KB". */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** One skill's detail as a pure view of the engine read: description, where it applies and
 * comes from, its switch, its SKILL.md body as agents read it and its files. Inputs: the
 * read, the project id when shown in a project, the last failure and the actions. */
export function SkillDetailView({ detail, projectId, failure, onToggle, onRetry }: {
  detail: Loaded<SkillDetail>;
  projectId?: string;
  failure: string;
  onToggle: (skill: SkillView, on: boolean) => void;
  onRetry: () => void;
}) {
  if (detail.kind === "loading") return <Skeleton shape="row" count={6} />;
  if (detail.kind === "not_ready") return <NotReady what="Skill details" compact />;
  if (detail.kind === "error") return <LoadError message={detail.message} onRetry={onRetry} />;
  const { skill, text, files, files_more } = detail.value;
  const on = pluginSwitch(skill, projectId);
  const extra = skillHeaderExtra(text, skill);
  return <div className="stack">
    <p className="secondary">{skill.description}</p>
    {skill.refused && <Notice tone="danger">{skillUnused(skill.refused)} Remove it in Settings, Plugins and skills.</Notice>}
    {extra.length > 0 && <Notice tone="warn"><span>Agents also read these lines of its SKILL.md header:</span>
      <pre className="mono small" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", margin: 0 }}>{extra.map(displayText).join("\n")}</pre></Notice>}
    <div className="row between">
      <span className="stack-small"><span className="secondary">{skill.scope_label}</span><span className="muted small">{skill.origin}</span><span className="mono muted small">{homePath(skill.source)}</span></span>
      <Switch id="skill-detail-on" label={on.label} checked={on.checked} onChecked={(next) => onToggle(skill, next)}
        disabledReason={on.project && !skill.enabled ? "Turned off for every project in Settings." : undefined} />
    </div>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    <Panel title="SKILL.md"><pre className="mono small" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", margin: 0 }}>{displayText(skillBody(text))}</pre></Panel>
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
  const [detail, reload] = useLoad(() => skillDetail(id, projectId), [id, projectId], "skill-detail");
  const [failure, setFailure] = useState("");
  const toggle = async (skill: SkillView, on: boolean) => {
    setFailure("");
    const target = pluginSwitch(skill, projectId);
    const result = session.noteResult<unknown>(target.project && projectId ? await switchPlugin(projectId, skill.id, on, skill.version) : await setSkillEnabled(skill.id, on, skill.version));
    if (result.kind === "ok") { session.showToast(on ? "Skill turned on." : "Skill turned off."); reload(); onChanged(); }
    else {
      setFailure(result.kind === "not_ready" ? NOT_READY : result.message);
      if (readsAgain(result)) { reload(); onChanged(); }
    }
  };
  const title = detail.kind === "ok" ? detail.value.skill.name : "Skill";
  return <Dialog open title={title} onClose={onClose} wide footer={<Button onClick={onClose}>Close</Button>}>
    <SkillDetailView detail={detail} projectId={projectId} failure={failure} onRetry={reload}
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
    {outcomes.map((outcome, index) => <Notice key={`${index}-${outcome.name}`} tone={outcome.result.kind === "ok" ? "ok" : "danger"}>{displayText(importSentence(outcome))}</Notice>)}
    {found.length === 0 ? <EmptyState icon="folder-input" title="No skills found in those folders" compact /> : <Panel padded={false}><div className="rows">
      {found.map((item) => {
        const word = importWord(item.state);
        const why = item.state.kind === "unusable" ? plainError(item.state.code)
          : item.state.kind === "differs" ? "Importing replaces the added copy and keeps its project switches." : "";
        return <div className="item-row" key={item.folder}>
          <Checkbox checked={ticked.has(item.folder)} disabled={!importable(item.state)} onChecked={(on) => onTick(item, on)}
            label={<span className="stack-small"><span className="title">{displayText(item.name)}</span>{item.line && <span className="secondary">{displayText(item.line)}</span>}
              <span className="mono muted small">{IMPORT_FROM[item.from]}: {displayText(homePath(item.folder))}</span>{why && <span className="secondary">{why}</span>}</span>} />
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
  const [expanded, setExpanded] = useState<string | null>(null);
  if (servers.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (servers.kind === "not_ready") return <NotReady what="MCP servers" compact />;
  if (servers.kind === "error") return <LoadError message="Couldn't load MCP servers." onRetry={onRetry} />;
  const add = <Button icon="plus" onClick={onAdd}>Add MCP server</Button>;
  const line = (server: McpServerView, index: number) => {
    const on = pluginSwitch(server, projectId);
    const command = server.spec.kind === "stdio";
    const waiting = server.status === "waiting_for_approval";
    const broken = server.status === "cant_start";
    const check = broken ? "Check again" : "Check";
    const checking = busy === `check:${server.id}`;
    const open = expanded === server.id;
    return <Fragment key={server.id}>
      <SettingLine label={server.name} changed={pluginChanged(server, projectId)} shown={<>{server.name} <StatusBadge word={serverWord(server.status)} /></>} help={<>
        {!waiting && <span className="mono small">{server.runs}</span>}
        {waiting && (command ? <span>Runs: <span className="mono">{server.runs}</span></span>
          : <span>Connects to: <span className="mono">{server.spec.kind === "http" ? server.spec.url : ""}</span></span>)}
        {waiting && serverFacts(server).slice(1).map((fact) => <span key={fact}>{fact}</span>)}
        {broken && <span className="field-error">Can't start: {server.problem ?? "the server didn't answer."}</span>}
        {server.tools.length > 0 && <span><Button size="small" variant="quiet" icon={open ? "chevron-down" : "chevron-right"} aria-expanded={open} onClick={() => setExpanded(open ? null : server.id)}>
          {server.tools.length} {server.tools.length === 1 ? "tool" : "tools"}
        </Button></span>}
      </>}>
        <Switch id={`server-on-${index}`} label={on.label} checked={on.checked} onChecked={(next) => onToggle(server, next)}
          disabledReason={on.project && !server.enabled ? "Turned off for every project in Settings." : undefined} />
        {command && <Button aria-label={`${check} ${server.name}`} pending={checking} pendingLabel="Checking..." disabled={busy !== null} onClick={() => onCheck(server)}>{check}</Button>}
        {waiting && <Button variant="primary" icon="shield-check" aria-label={`Approve ${server.name}`} pending={busy === server.id} pendingLabel="Approving..." disabled={busy !== null} onClick={() => onApprove(server)}>Approve</Button>}
        {!on.project && <Button aria-label={`Secrets for ${server.name}`} onClick={() => onSecrets(server)}>Secrets</Button>}
        {!on.project && <Button variant="quiet" aria-label={`Remove ${server.name}`} onClick={() => onRemove(server)}>Remove</Button>}
      </SettingLine>
      {open && <div className="review"><ToolSwitches projectId={projectId} serverId={server.id} /></div>}
    </Fragment>;
  };
  return <SettingSection title="MCP servers" actions={servers.value.length > 0 ? add : undefined}
    about="MCP servers give agents extra tools. The engine checks each server, and you approve it before agents can use it.">
    {failure && <div className="set-body"><Notice tone="danger" alert>{failure}</Notice></div>}
    {servers.value.length === 0 ? <div className="set-body"><EmptyState icon="plug" title="No MCP servers yet" compact action={add} /></div> : servers.value.map(line)}
  </SettingSection>;
}

/** What one hook row says it runs on: its event and, when set, its matcher.
 * Input: the hook. Output: the line. */
export function hookTrigger(hook: HookView): string {
  return hook.matcher ? `${hook.event}, matching ${hook.matcher}` : hook.event;
}

/** One plugin's hooks as the Hooks list groups them: a key for the plugin and scope, the
 * plugin's name, how many distinct events it runs on, the skills or servers it runs with
 * and its hooks in the engine's order. */
export type HookGroup = { key: string; plugin: string; events: number; follows: string[]; hooks: HookView[] };

/** Groups hooks per plugin and scope in the order the engine lists them, so a plugin with
 * several events reads as one plugin and not as several installs. Input: the hooks.
 * Output: the groups, together holding every hook once. */
export function hookGroups(hooks: HookView[]): HookGroup[] {
  const groups = new Map<string, HookGroup>();
  for (const hook of hooks) {
    const key = `${hook.plugin}\n${hook.scope.kind === "project" ? hook.scope.project_id : ""}`;
    const group = groups.get(key) ?? { key, plugin: hook.plugin, events: 0, follows: [], hooks: [] };
    if (!group.hooks.some((seen) => seen.event === hook.event)) group.events += 1;
    if (hook.follows && !group.follows.includes(hook.follows)) group.follows.push(hook.follows);
    group.hooks.push(hook);
    groups.set(key, group);
  }
  return [...groups.values()];
}

/** The name a plugin's hooks row shows: the plugin and how many events it runs on.
 * Input: the group. Output: the name, such as "caveman - 4 events". */
export function hookGroupName(group: HookGroup): string {
  return `${group.plugin} - ${group.events} ${group.events === 1 ? "event" : "events"}`;
}

/** What a plugin's hooks switch shows: on when every hook it can switch is on, "Partly on"
 * when only some are. In a project, hooks turned off for every project in Settings are not
 * the project's to switch and are left out. Inputs: the group and the project id, if any.
 * Output: checked, label and whether the project switch applies. */
export function hookGroupSwitch(group: HookGroup, projectId?: string): { checked: boolean; label: string; project: boolean } {
  const first = pluginSwitch(group.hooks[0], projectId);
  const each = group.hooks.map((hook) => pluginSwitch(hook, projectId)).filter((one, at) => !(one.project && !group.hooks[at].enabled));
  const on = each.filter((one) => one.checked).length;
  return { checked: each.length > 0 && on === each.length, label: on > 0 && on < each.length ? "Partly on" : first.label, project: first.project };
}

/** The hooks a plugin row's switch changes: those not already in the new state, leaving
 * out, in a project, hooks turned off for every project in Settings. Inputs: the group,
 * the new state and the project id, if any. Output: the hooks to set, in order. */
export function hooksToSet(group: HookGroup, on: boolean, projectId?: string): HookView[] {
  return group.hooks.filter((hook) => {
    const target = pluginSwitch(hook, projectId);
    return target.checked !== on && !(target.project && !hook.enabled);
  });
}

/** The events of one plugin's hooks, each with its trigger, command and own switch.
 * Inputs: the group, its place in the list, the project id when shown in a project and
 * the action that turns one hook on or off. */
export function HookEvents({ group, index, projectId, onToggle }: {
  group: HookGroup;
  index: number;
  projectId?: string;
  onToggle: (hook: HookView, on: boolean) => void;
}) {
  return <div className="review">{group.hooks.map((hook, at) => {
    const on = pluginSwitch(hook, projectId);
    return <SettingLine key={hook.id} label={hookTrigger(hook)} changed={pluginChanged(hook, projectId)} help={<span className="mono small">{hook.command}</span>}>
      <Switch id={`hook-on-${index}-${at}`} label={on.label} checked={on.checked} onChecked={(next) => onToggle(hook, next)}
        disabledReason={on.project && !hook.enabled ? "Turned off for every project in Settings." : undefined} />
    </SettingLine>;
  })}</div>;
}

/** The hooks list as a pure view of the engine read: one row per plugin with its events
 * count, follow note and switch, and its events under the row once opened. Inputs: the
 * read, the project id when shown in a project, the last failure and the actions. */
export function HooksView({ hooks, projectId, failure, onAdd, onToggle, onToggleGroup, onRetry }: {
  hooks: Loaded<HookView[]>;
  projectId?: string;
  failure: string;
  onAdd: () => void;
  onToggle: (hook: HookView, on: boolean) => void;
  onToggleGroup: (group: HookGroup, on: boolean) => void;
  onRetry: () => void;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);
  if (hooks.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (hooks.kind === "not_ready") return <NotReady what="Hooks" compact />;
  if (hooks.kind === "error") return <LoadError message="Couldn't load hooks." onRetry={onRetry} />;
  const add = <Button icon="folder-plus" onClick={onAdd}>Add hooks from a plugin</Button>;
  return <SettingSection title="Hooks" actions={hooks.value.length > 0 ? add : undefined}
    about={`Hooks are commands Claude Code and Codex agents run on events such as session start or a tool call. ${CODEX_HOOK_EVENTS_ONLY} ${BARE_NO_HOOKS} ${RCLI_NO_HOOKS} They run inside the agent's sandbox. A hook that came with a skill or MCP server runs only while that item is on.`}>
    {failure && <div className="set-body"><Notice tone="danger" alert>{failure}</Notice></div>}
    {hooks.value.length === 0 ? <div className="set-body"><EmptyState icon="zap" title="No hooks yet" compact action={add} /></div> : hookGroups(hooks.value).map((group, index) => {
      const on = hookGroupSwitch(group, projectId);
      const open = expanded === group.key;
      return <Fragment key={group.key}>
        <SettingLine label={hookGroupName(group)} changed={group.hooks.some((hook) => pluginChanged(hook, projectId))} help={<>
          {group.follows.length > 0 && <span>Runs while {group.follows.join(", ")} is on.</span>}
          <span><Button size="small" variant="quiet" icon={open ? "chevron-down" : "chevron-right"} aria-expanded={open} aria-label={`${open ? "Hide" : "Show"} ${group.plugin} events`}
            onClick={() => setExpanded(open ? null : group.key)}>{open ? "Hide events" : "Show events"}</Button></span>
        </>}>
          <Switch id={`hook-on-${index}`} label={on.label} checked={on.checked} onChecked={(next) => onToggleGroup(group, next)}
            disabledReason={on.project && group.hooks.every((hook) => !hook.enabled) ? "Turned off for every project in Settings." : undefined} />
        </SettingLine>
        {open && <HookEvents group={group} index={index} projectId={projectId} onToggle={onToggle} />}
      </Fragment>;
    })}
  </SettingSection>;
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
    const picked = await pickFolder("skill");
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
  id: number;
  name: string;
  value: RefObject<HTMLInputElement | null>;
}

/** Reads each secret row's name with the value in that row's own masked field.
 * Input: the rows. Output: name and value pairs in row order. */
export function readSecrets(rows: SecretRow[]): { name: string; value: string }[] {
  return rows.map((row) => ({ name: row.name, value: row.value.current?.value ?? "" }));
}

function clearSecrets(rows: SecretRow[]): void {
  for (const row of rows) if (row.value.current) row.value.current.value = "";
}

/** The secret rows of Add MCP server: a name and a masked value each, keyed by an id that never
 * changes, so removing one row never moves a typed value onto another row's name. Add secret
 * stops at the secrets limit. Inputs: the rows and their change handler. */
export function SecretRows({ rows, onRows }: { rows: SecretRow[]; onRows: (rows: SecretRow[]) => void }) {
  return <>
    {rows.map((row) => <div className="row" key={row.id}>
      <TextField id={`server-secret-name-${row.id}`} label="Secret name" value={row.name} spellCheck={false} autoComplete="off"
        onValue={(value) => onRows(rows.map((other) => other.id === row.id ? { ...other, name: value } : other))} />
      <SecretField id={`server-secret-value-${row.id}`} label="Value" inputRef={row.value} />
      <Button iconOnly="Remove secret" icon="trash" onClick={() => { if (row.value.current) row.value.current.value = ""; onRows(rows.filter((other) => other.id !== row.id)); }} />
    </div>)}
    {rows.length < SECRETS_MAX && <span><Button size="small" icon="plus"
      onClick={() => onRows([...rows, { id: Math.max(0, ...rows.map((other) => other.id)) + 1, name: "", value: createRef<HTMLInputElement>() }])}>Add secret</Button></span>}
  </>;
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
        <SecretRows rows={rows} onRows={setRows} />
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
  const [section, setSection] = useState<"skills" | "servers" | "hooks" | "tools">("skills");
  const [skills, reloadSkills] = useLoad(() => listSkills(projectId), [projectId], "skills");
  const [servers, reloadServers] = useLoad(() => listMcpServers(projectId), [projectId], "mcp-servers");
  const [hooks, reloadHooks] = useLoad(() => listHooks(projectId), [projectId], "hooks");
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
    else { setFailure(result.kind === "not_ready" ? NOT_READY : result.message); if (readsAgain(result)) reload(); }
  }

  const addSkillFromChooser = async () => {
    setFailure("");
    const picked = await pickFolder("skill");
    if (picked.kind !== "ok") return setAdding({ kind: "skill", message: picked.kind === "not_ready" ? "The folder chooser isn't ready in this build. Type the full path instead." : picked.message });
    const folder = picked.value;
    if (folder === null) return;
    await act(folder, () => addSkill(scope, folder), (skill) => `Added ${skill.name}.`, reloadSkills);
  };

  const toggleSkill = (skill: SkillView, on: boolean) => {
    const target = pluginSwitch(skill, projectId);
    void act<unknown>(skill.id, () => target.project && projectId ? switchPlugin(projectId, skill.id, on, skill.version) : setSkillEnabled(skill.id, on, skill.version),
      () => on ? "Skill turned on." : "Skill turned off.", reloadSkills);
  };
  const addHooksFromChooser = async () => {
    setFailure("");
    const picked = await pickFolder("plugin");
    if (picked.kind !== "ok") return setFailure(picked.kind === "not_ready" ? "The folder chooser isn't ready in this build." : picked.message);
    const folder = picked.value;
    if (folder === null) return;
    await act(folder, () => addHooks(scope, folder), (added) => `Added ${added.length} ${added.length === 1 ? "hook" : "hooks"}.`, reloadHooks);
  };
  const setHook = (hook: HookView, on: boolean): Promise<Result<unknown>> =>
    pluginSwitch(hook, projectId).project && projectId ? switchPlugin(projectId, hook.id, on, hook.version) : setHookEnabled(hook.id, on);
  const toggleHook = (hook: HookView, on: boolean) => {
    void act<unknown>(hook.id, () => setHook(hook, on), () => on ? "Hook turned on." : "Hook turned off.", reloadHooks);
  };
  const toggleHookGroup = (group: HookGroup, on: boolean) => {
    const changing = hooksToSet(group, on, projectId);
    void act<unknown>(group.key, async () => {
      for (const [at, hook] of changing.entries()) {
        const result = await setHook(hook, on);
        if (result.kind !== "ok") { if (at > 0) reloadHooks(); return result; }
      }
      return { kind: "ok", value: null };
    }, () => `${group.plugin} hooks turned ${on ? "on" : "off"}.`, reloadHooks);
  };
  const toggleServer = (server: McpServerView, on: boolean) => {
    const target = pluginSwitch(server, projectId);
    void act<unknown>(server.id, () => target.project && projectId ? switchPlugin(projectId, server.id, on, server.version) : setMcpServerEnabled(server.id, on, server.version),
      () => on ? "Server turned on." : "Server turned off.", reloadServers);
  };
  const confirmRemove = () => {
    if (!removing) return;
    const item = removing;
    setRemoving(undefined);
    if (item.kind === "skill") void act(item.skill.id, () => removeSkill(item.skill.id, item.skill.version), () => "Skill removed.", reloadSkills);
    else void act(item.server.id, () => removeMcpServer(item.server.id, item.server.version), () => "Server removed.", reloadServers);
  };

  const picker = <Segmented label="Plugins and skills" value={section} onValue={(next) => { setFailure(""); setSection(next); }}
    options={[{ value: "skills", label: "Skills", icon: "book-open" }, { value: "servers", label: "MCP servers", icon: "plug" }, { value: "hooks", label: "Hooks", icon: "zap" }, { value: "tools", label: "Tools", icon: "wrench" }]} />;

  return <div className="stack">
    {projectId ? picker : <div className="settings-top"><h2>Plugins and skills</h2>{picker}</div>}
    {section === "tools" && <ToolsSection projectId={projectId} onAddTool={() => setAdding({ kind: "server" })} />}
    {section === "hooks" && <HooksView hooks={hooks} projectId={projectId} failure={failure} onRetry={reloadHooks} onAdd={() => void addHooksFromChooser()} onToggle={toggleHook} onToggleGroup={toggleHookGroup} />}
    {section === "hooks" || section === "tools" ? null : section === "skills"
      ? <SkillsView skills={skills} projectId={projectId} failure={failure} onRetry={reloadSkills} onAdd={() => void addSkillFromChooser()}
        onImport={projectId ? undefined : () => setImporting(true)} onOpen={(item) => open(item.id)} onToggle={toggleSkill} onRemove={(skill) => setRemoving({ kind: "skill", skill })} />
      : <ServersView servers={servers} projectId={projectId} failure={failure} busy={busy} onRetry={reloadServers} onAdd={() => setAdding({ kind: "server" })}
        onToggle={toggleServer} onSecrets={setSecrets} onRemove={(server) => setRemoving({ kind: "server", server })}
        onApprove={(server) => void act(server.id, () => approveMcpServer(server.id, server.spec), () => "Server approved.", reloadServers)}
        onCheck={(server) => void act(`check:${server.id}`, () => checkMcpServer(server.id),
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
