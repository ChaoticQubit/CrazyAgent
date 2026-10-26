import { useState } from "react";
import { delegateTeamTask, keepTeamWarm } from "../../api";
import type { Result } from "../../api";
import { failureCopy, receiptState } from "../../api-projects";
import type { IntentReceipt, TeamStatus } from "../../engine-client";
import { Button, Select, Switch, keyLabel, shortcutKey } from "../../ui";
import type { SelectOption } from "../../ui";
import { displayText } from "../conversation";

export const TASK_PROMPT_BYTES_MAX = 4000;
const ANY_HARNESS = "";
const encoder = new TextEncoder();

/** The harnesses a task can go to: every sub-agent harness of the team plan, once each, plus
 * "Any sub-agent" first. Inputs: the team status and a harness name lookup. Output: options. */
export function delegateOptions(status: Pick<TeamStatus, "harnesses">, harness: (id: string) => string = (id) => id): SelectOption[] {
  const subs = [...new Set(status.harnesses.filter((item) => item.role === "sub").map((item) => item.harness))];
  return [{ value: ANY_HARNESS, label: "Any sub-agent" }, ...subs.map((id) => ({ value: id, label: harness(id) }))];
}

/** Checks a task before it is handed to a sub-agent, with the engine's own rules: not blank,
 * no control characters but tab and newline, at most 4000 bytes. Input: the text. Output: the
 * trimmed task, or the plain reason it can't be sent. */
export function taskInput(text: string): { kind: "ok"; prompt: string } | { kind: "invalid"; reason: string } {
  const prompt = text.trim();
  if (!prompt) return { kind: "invalid", reason: "Write the task first." };
  if (/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(prompt)) return { kind: "invalid", reason: "Remove the control characters from the task." };
  const bytes = encoder.encode(prompt).length;
  if (bytes > TASK_PROMPT_BYTES_MAX) return { kind: "invalid", reason: `Too long: ${bytes} of ${TASK_PROMPT_BYTES_MAX} bytes.` };
  return { kind: "ok", prompt };
}

/** What a team change's result means for the owner. Inputs: the call result, what it did and
 * the "Not ready yet" subject. Output: the note to show, empty when it applied. */
export function changeNote(result: Result<IntentReceipt>, missing: string): string {
  if (result.kind === "not_ready") return `Not ready yet: ${missing} isn't in this build.`;
  if (result.kind === "error") return failureCopy(result);
  const state = receiptState(result.value);
  if (state.kind === "applied") return "";
  return state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the team before trying again.";
}

/** Team controls on the Agents tab: keep the plan's minimum sub-agents ready, and hand one task
 * straight to a sub-agent when the master should not plan it. Inputs: the project id, the
 * latest team status, a harness name lookup and a reload. Output: the controls. */
export function TeamControls({ projectId, status, harness, reload }: {
  projectId: string;
  status: TeamStatus;
  harness?: (id: string) => string;
  reload: () => void;
}) {
  const [warm, setWarm] = useState<boolean | null>(null);
  const [target, setTarget] = useState(ANY_HARNESS);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [sent, setSent] = useState("");
  const options = delegateOptions(status, harness);
  const checked = warm ?? status.keep_warm;
  const check = taskInput(text);
  const subs = options.length > 1;

  async function toggle(on: boolean) {
    setWarm(on);
    setBusy(true);
    const message = changeNote(await keepTeamWarm(projectId, on), "keeping agents ready");
    setBusy(false);
    setWarm(null);
    setNote(message);
    reload();
  }

  async function hand() {
    if (check.kind !== "ok" || busy) return;
    setBusy(true);
    const message = changeNote(await delegateTeamTask(projectId, target || null, check.prompt), "handing tasks to sub-agents");
    setBusy(false);
    setNote(message);
    if (!message) {
      setText("");
      setSent("Task handed off. It shows in the graph and the task list.");
    }
    reload();
  }

  return <section className="tg-controls" aria-label="Team controls">
    <Switch id={`tg-warm-${projectId}`} label={`Keep ${status.min_agents} ${status.min_agents === 1 ? "sub-agent" : "sub-agents"} ready`} checked={checked}
      onChecked={(on) => void toggle(on)} disabledReason={busy ? "Saving the last change." : status.min_agents < 1 ? "The team plan keeps no sub-agents ready." : undefined} />
    {subs ? <form className="tg-hand" onSubmit={(event) => { event.preventDefault(); void hand(); }}>
      <h4>Hand a task to a sub-agent</h4>
      <Select id={`tg-hand-to-${projectId}`} label="Send to" value={target} options={options} onValue={setTarget} />
      <label htmlFor={`tg-hand-text-${projectId}`}>Task</label>
      <textarea id={`tg-hand-text-${projectId}`} className="pj-input" rows={3} value={text} onChange={(event) => { setText(event.target.value); setSent(""); }}
        aria-describedby={`tg-hand-help-${projectId}`} onKeyDown={(event) => { if (event.key === "Enter" && shortcutKey(event)) { event.preventDefault(); void hand(); } }} />
      <div className="tg-composer-foot">
        <span className="tg-terminal-note" id={`tg-hand-help-${projectId}`}>{text && check.kind === "invalid" ? check.reason : keyLabel("Skips the master. The master sees the task and its result. Cmd+Enter to send.")}</span>
        <Button type="submit" variant="primary" size="small" icon="send" disabled={check.kind !== "ok" || busy} pending={busy} pendingLabel="Handing off...">Hand off</Button>
      </div>
    </form> : <p className="tg-terminal-note">This team has no sub-agents. Add some in the project settings to hand tasks to them.</p>}
    {sent && <p className="pj-note" role="status">{sent}</p>}
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
  </section>;
}
