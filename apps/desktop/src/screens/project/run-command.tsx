import { useState } from "react";
import type { Result } from "../../api";
import { runCopy, runInProjectTerminal } from "../../api-ide";
import { Button, Dialog, navigate } from "../../ui";
import { copyText } from "../team/terminal-panel";

/** The most bytes one command typed into a project terminal may hold, as the engine counts
 * them: UTF-8. */
export const COMMAND_BYTES_MAX = 512;

/** What "Run in project terminal" does for a project: types the command and presses Enter,
 * types it and leaves Enter to the owner, or is off so only Copy shows. */
export type RunMode = "type_and_run" | "type_only" | "off";

const EMPTY = "ide_run_empty";
const TOO_LONG = "ide_run_too_long";
const NOT_ONE_LINE = "ide_run_not_one_line";
const CONTROL = "ide_run_control_character";
const CHAINED = "ide_run_chained";
const BLANK = /^[\t-\r \u0085\u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}]*$/u;
const HIDDEN = /[\p{Cc}\u{ad}\u{61c}\u{180e}\u{200b}-\u{200f}\u{2028}-\u{202e}\u{2060}-\u{206f}\u{feff}\u{fff9}-\u{fffb}\u{e0000}-\u{e007f}]/u;
const CHAINING = [";", "|", "&", "`", "<", ">", "!", "$("];
const REASONS: Record<string, string> = {
  [TOO_LONG]: "This line is too long to run from here. Copy it and run it yourself.",
  [NOT_ONE_LINE]: "This is more than one line. Copy it and run it yourself.",
  [CONTROL]: "This line holds a hidden character. Copy it and run it yourself.",
  [CHAINED]: "This line chains commands. Copy it and run it yourself.",
};
const RUNS = "It runs in this project's terminal, inside the project sandbox.";
const TYPED = "It is typed into this project's terminal. You press Enter.";
const WENT = "Typed into a new terminal in the IDE tab.";
const NOT_READY = "CrazyAgent's engine isn't ready yet. Try again in a moment.";
const COPIED = "Copied.";
const COPY_REFUSED = "The clipboard refused the copy.";
const LINE_BOX = { margin: 0, padding: 12, borderRadius: "var(--radius-control)", background: "var(--surface-2)", whiteSpace: "pre-wrap" } as const;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Says why a command may not be typed into a project terminal, or that it may: the engine's
 * rule, mirrored so the Run button hides where the engine would refuse. Input: the command as
 * shown. Output: null for one plain single command, else the engine's refusal code. */
export function commandProblem(command: string): string | null {
  if (BLANK.test(command)) return EMPTY;
  const bytes = new TextEncoder().encode(command).length;
  if (bytes > COMMAND_BYTES_MAX) return TOO_LONG;
  if (/[\n\r]/.test(command)) return NOT_ONE_LINE;
  if (HIDDEN.test(command)) return CONTROL;
  if (CHAINING.some((mark) => command.includes(mark)) || command.trimStart().startsWith("^")) return CHAINED;
  assert(bytes >= 1 && bytes <= COMMAND_BYTES_MAX, "a command that passed the rule left its size bound");
  assert(!command.includes("\n"), "a command that passed the rule holds a line break");
  return null;
}

/** The confirm every run goes through: the command verbatim in a monospace box, what
 * confirming does in this mode, focus on Cancel. Inputs: the command, the mode, open, pending
 * and the two handlers. Output: the dialog, or nothing while closed. */
export function RunConfirm({ command, mode, open, pending, onConfirm, onCancel }: {
  command: string;
  mode: RunMode;
  open: boolean;
  pending: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  if (!open) return null;
  assert(mode !== "off", "the run confirm opened in the Off mode");
  assert(commandProblem(command) === null, "the run confirm opened for a refused command");
  return <Dialog open title="Run this command?" onClose={onCancel} busy={pending}
    footer={<><Button disabled={pending} onClick={onCancel} data-autofocus>Cancel</Button><Button variant="primary" pending={pending} onClick={onConfirm}>{mode === "type_only" ? "Type it" : "Run"}</Button></>}>
    <pre className="pj-mono" style={LINE_BOX}>{command}</pre>
    <p>{mode === "type_only" ? TYPED : RUNS}</p>
  </Dialog>;
}

/** Sends one confirmed command and says what happened in a plain sentence. Inputs: the
 * project, the command and the run call. Output: the note, and whether to offer the IDE tab
 * because the project's IDE isn't open; a call that throws reads as not ready. */
export async function runOutcome(projectId: string, command: string, run: (projectId: string, command: string) => Promise<Result<void>>): Promise<{ note: string; openIde: boolean }> {
  assert(commandProblem(command) === null, "a refused command reached the run call");
  const result = await run(projectId, command).catch((): Result<void> => ({ kind: "not_ready" }));
  if (result.kind === "ok") return { note: WENT, openIde: false };
  if (result.kind === "not_ready") return { note: NOT_READY, openIde: false };
  return { note: runCopy(result), openIde: result.code === "ide_not_running" };
}

/** The actions beside one command line: Copy always, and "Run in project terminal" when the
 * mode is not Off and the line is one plain command; a refused line says why once. Run opens
 * the confirm every time and sends the command only on Confirm; after a run the row says
 * where the command went, or why not, and offers the IDE tab when the IDE isn't open.
 * Inputs: the project, the command, the mode, and the run and copy calls. Output: the row. */
export function RunCommand({ projectId, command, mode, run = runInProjectTerminal, copy = copyText }: {
  projectId: string;
  command: string;
  mode: RunMode;
  run?: (projectId: string, command: string) => Promise<Result<void>>;
  copy?: (text: string) => Promise<boolean>;
}) {
  const [asking, setAsking] = useState(false);
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [openIde, setOpenIde] = useState(false);
  const problem = commandProblem(command);
  const runnable = mode !== "off" && problem === null;
  const reason = mode !== "off" && problem !== null ? REASONS[problem] : undefined;
  const confirmed = async () => {
    assert(runnable, "a run was confirmed for a line with no Run button");
    assert(!pending, "a run was confirmed while another was still sent");
    setPending(true);
    const outcome = await runOutcome(projectId, command, run);
    setPending(false);
    setAsking(false);
    setNote(outcome.note);
    setOpenIde(outcome.openIde);
  };
  return <div className="pj-row">
    <Button size="small" icon="copy" onClick={async () => setNote(await copy(command) ? COPIED : COPY_REFUSED)}>Copy</Button>
    {runnable && <Button size="small" icon="terminal" onClick={() => { setNote(null); setOpenIde(false); setAsking(true); }}>Run in project terminal</Button>}
    {reason && <span className="pj-small pj-muted">{reason}</span>}
    <span className="pj-small pj-muted" role="status">{note}</span>
    {openIde && <Button size="small" variant="quiet" onClick={() => navigate({ name: "project", id: projectId, tab: "ide" })}>Open the IDE tab</Button>}
    {runnable && <RunConfirm command={command} mode={mode} open={asking} pending={pending} onConfirm={() => void confirmed()} onCancel={() => setAsking(false)} />}
  </div>;
}
