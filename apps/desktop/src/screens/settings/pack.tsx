import { useEffect, useState } from "react";
import { intakeCopy, loadSetupStatus, removePackEntry, runSetup, setPackEntry } from "../../api-intake";
import type { PackEntry, SetupStatus } from "../../engine-client";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, Icon, LoadError, NotReady, Skeleton, StatusBadge, Switch } from "../../ui";
import { displayText } from "../conversation";
import { SETUP_POLL_MS, entryIcon, entryName, installed, packGroups, packStateView, setupBusy } from "../intake/intake";
import "../project/projects.css";
import "../intake/intake.css";

export type PackAsk = { kind: "remove" | "install"; entry: PackEntry };

/** Words for the confirm before a pack change: removing deletes what the entry added, and
 * installing an MCP server lets agents run it. Input: the pending change. Output: title, body
 * and confirm label. */
export function packConfirm(ask: PackAsk): { title: string; body: string; confirm: string } {
  const name = entryName(ask.entry);
  if (ask.kind === "remove") {
    return { title: `Remove ${name}?`, confirm: "Remove",
      body: `CrazyAgent deletes the ${ask.entry.kind === "mcp_server" ? "MCP server" : "skill"} it installed for every project. You can install it again from this page.` };
  }
  return { title: `Install ${name}?`, confirm: "Install",
    body: ask.entry.kind === "mcp_server" ? `Installing it lets your agents run this MCP server in every project. It comes from ${displayText(ask.entry.source)}.`
      : `Agents in every project can use this skill. It comes from ${displayText(ask.entry.source)}.` };
}

/** Settings, Pack as a view of its state: the default pack grouped like setup, a switch on each
 * installed entry, Remove on each removable one and Install on each that isn't installed.
 * Inputs: the setup read, the entry being changed, the last problem and the handlers. */
export function PackSettingsView({ status, busy, problem, onSwitch, onAsk, onRetry }: {
  status: Loaded<SetupStatus>;
  busy: string | null;
  problem: string;
  onSwitch: (entry: PackEntry, on: boolean) => void;
  onAsk: (ask: PackAsk) => void;
  onRetry: () => void;
}) {
  if (status.kind === "loading") return <Skeleton shape="row" count={6} />;
  if (status.kind === "not_ready") return <NotReady what="The default pack" />;
  if (status.kind === "error") return <LoadError message={status.message} onRetry={onRetry} />;
  const running = setupBusy(status.value);
  return <div className="pj pj-stack">
    <p className="pj-secondary">The skills and plugins every project starts with. Switching one off stops agents using it; removing it deletes what it installed.</p>
    {running && <p className="pj-secondary" role="status">Setup is installing. Changes wait until it finishes.</p>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    {packGroups(status.value.pack).map((group) => <section className="pj-stack-s" key={group.title} aria-label={group.title}>
      <h2>{group.title}</h2>
      {group.about && <p className="pj-help">{group.about}</p>}
      <ul className="in-list">{group.entries.map((entry) => {
        const state = packStateView(entry.state);
        const required = entry.group === "always";
        const locked = running || busy !== null;
        return <li className="in-row" key={entry.id}>
          <span className="in-row-main">
            <span className="pj-row"><Icon name={entryIcon(entry)} size={14} /><strong>{entryName(entry)}</strong>{entry.kind === "mcp_server" && <span className="pj-small pj-muted">MCP server</span>}
              <StatusBadge word={state.word} tone={state.tone} /></span>
            <span className="pj-small pj-muted">{displayText(entry.source)}, {displayText(entry.license)}</span>
            {state.line && <span className="pj-small pj-error">{state.line}</span>}
          </span>
          <span className="pj-row">
            {installed(entry) && <Switch id={`pack-${entry.id}`} label={`Use ${entryName(entry)}`} hideLabel checked={entry.state.state === "on"}
              disabledReason={required ? "Always on." : locked ? "Wait for the current change." : undefined} onChecked={(on) => onSwitch(entry, on)} />}
            {installed(entry) && !required && <Button size="small" variant="danger-text" disabled={locked} onClick={() => onAsk({ kind: "remove", entry })}>Remove</Button>}
            {!installed(entry) && entry.state.state !== "installing" && <Button size="small" disabled={locked} pending={busy === entry.id}
              pendingLabel="Starting..." onClick={() => onAsk({ kind: "install", entry })}>{entry.state.state === "failed" ? "Try again" : "Install"}</Button>}
          </span>
        </li>;
      })}</ul>
    </section>)}
  </div>;
}

/** Settings, Pack: reads the pack, switches, removes and installs entries, and reads again every
 * few seconds while setup installs. */
export function PackSettings() {
  const session = useAppSession();
  const [loaded, reload] = useLoad(loadSetupStatus);
  const [fresh, setFresh] = useState<SetupStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [ask, setAsk] = useState<PackAsk | null>(null);
  const [problem, setProblem] = useState("");
  const status: Loaded<SetupStatus> = fresh ? { kind: "ok", value: fresh } : loaded;
  const running = status.kind === "ok" && setupBusy(status.value);

  useEffect(() => { if (loaded.kind === "ok") setFresh(loaded.value); else setFresh(null); }, [loaded]);
  useEffect(() => {
    if (!running) return;
    const timer = window.setTimeout(reload, SETUP_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [running, status]);

  const apply = async (entry: PackEntry, change: () => ReturnType<typeof setPackEntry>, done: string) => {
    if (busy) return;
    setBusy(entry.id);
    setProblem("");
    const result = await change();
    setBusy(null);
    setAsk(null);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Changing the pack isn't in this build." : intakeCopy(result.code)); reload(); return; }
    setFresh(result.value);
    session.showToast(done);
  };

  const confirm = (pending: PackAsk) => {
    const name = entryName(pending.entry);
    if (pending.kind === "remove") void apply(pending.entry, () => removePackEntry(pending.entry.id), `${name} was removed.`);
    else void apply(pending.entry, () => runSetup([pending.entry.id]), `Installing ${name}.`);
  };

  const words = ask ? packConfirm(ask) : null;
  return <>
    <PackSettingsView status={status} busy={busy} problem={problem} onRetry={reload} onAsk={setAsk}
      onSwitch={(entry, on) => void apply(entry, () => setPackEntry(entry.id, on), `${entryName(entry)} is ${on ? "on" : "off"}.`)} />
    <ConfirmDialog open={ask?.kind === "remove"} title={words?.title ?? ""} confirmLabel={words?.confirm ?? "Remove"} pending={busy !== null}
      onCancel={() => setAsk(null)} onConfirm={() => { if (ask) confirm(ask); }}>
      {words?.body}
    </ConfirmDialog>
    <Dialog open={ask?.kind === "install"} title={words?.title ?? ""} onClose={() => setAsk(null)}
      footer={<><Button onClick={() => setAsk(null)}>Cancel</Button>
        <Button variant="primary" pending={busy !== null} pendingLabel="Starting..." onClick={() => { if (ask) confirm(ask); }}>{words?.confirm ?? "Install"}</Button></>}>
      <p>{words?.body}</p>
    </Dialog>
  </>;
}
