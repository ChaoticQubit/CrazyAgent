import { useEffect, useState, type FormEvent } from "react";
import { getSandboxSettings, saveSandboxSettings, type LinuxSockets, type SandboxSettings } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, Panel, RadioCard, Skeleton } from "../../ui";

/** The Linux limited-network sockets options in the order Settings lists them, each with its
 * title and the plain sentence naming what it allows and what it risks. */
export const LINUX_SOCKETS: { value: LinuxSockets; title: string; detail: string }[] = [
  {
    value: "tcp_only",
    title: "TCP only, names looked up over TCP (recommended)",
    detail: "Only TCP to the model's ports; names are looked up over TCP. A harness whose resolver ignores RES_OPTIONS, such as a musl build, cannot look names up, so it does not start.",
  },
  {
    value: "refuse",
    title: "Do not start limited-network agents",
    detail: "No agent limited to its model starts on Linux, so the master is unavailable there.",
  },
  {
    value: "tcp_ports",
    title: "TCP ports only",
    detail: "Only the model's TCP ports are limited. Hosts and UDP are not limited: UDP, ping and other sockets reach any host.",
  },
];

/** Settings, Sandbox as a view of its state: the Linux limited-network sockets choice with Save
 * changes, enabled only after a change. Inputs: the settings read, the picked option, a saving
 * flag, the last problem and the handlers. Output: the panel. */
export function SandboxSettingsView({ settings, picked, saving, problem, onPick, onSave, onRetry }: {
  settings: Loaded<SandboxSettings>;
  picked: LinuxSockets | null;
  saving: boolean;
  problem: string;
  onPick: (value: LinuxSockets) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <Skeleton shape="line" count={4} />;
  if (settings.kind === "not_ready") return <NotReady what="The sandbox settings" />;
  if (settings.kind === "error") return <LoadError message={`Couldn't load the sandbox settings. ${settings.message}`} onRetry={onRetry} />;
  const chosen = picked ?? settings.value.linux_sockets;
  const changed = chosen !== settings.value.linux_sockets;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <Panel title="Linux limited-network sockets">
    <form className="form" aria-label="Linux limited-network sockets" noValidate onSubmit={submit}>
      <p className="helper">How an agent limited to its model reaches the network when CrazyAgent runs on Linux. Landlock limits TCP by port only.</p>
      <fieldset className="field" disabled={saving}>
        <legend className="field-label">Linux limited-network sockets</legend>
        {LINUX_SOCKETS.map((option) => <RadioCard key={option.value} name="linux-sockets" value={option.value} checked={chosen === option.value}
          onChecked={(value) => onPick(value as LinuxSockets)} title={option.title} details={[option.detail]} />)}
      </fieldset>
      {problem && <Notice tone="danger" alert>{problem}</Notice>}
      <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
    </form>
  </Panel>;
}

/** Settings, Sandbox: reads the owner's sandbox settings and saves the picked Linux
 * limited-network sockets option in the engine. */
export function SandboxSettingsTab() {
  const session = useAppSession();
  const [settings, reload] = useLoad(getSandboxSettings);
  const [picked, setPicked] = useState<LinuxSockets | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setPicked(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok" || picked === null) return;
    setSaving(true);
    setProblem("");
    const result = await saveSandboxSettings({ ...settings.value, linux_sockets: picked });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Sandbox setting saved.");
    reload();
  }
  return <div className="stack">
    <SandboxSettingsView settings={settings} picked={picked} saving={saving} problem={problem}
      onPick={(value) => { setPicked(value); setProblem(""); }} onSave={() => void save()} onRetry={reload} />
  </div>;
}
