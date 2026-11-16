import { useEffect, useState, type FormEvent } from "react";
import { getSandboxSettings, saveSandboxSettings, type LinuxSockets, type LinuxStrict, type LinuxSupervisor, type SandboxSettings } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, RadioCard, SettingLine, SettingSection, Skeleton } from "../../ui";
import "../project/projects.css";

/** The Linux limited-network sockets options in the order Settings lists them, each with its
 * title and the plain sentence naming what it allows and what it risks. */
export const LINUX_SOCKETS: { value: LinuxSockets; title: string; detail: string }[] = [
  {
    value: "tcp_only",
    title: "TCP only, names looked up over TCP (recommended)",
    detail: "Only TCP to the model's ports; names are looked up over TCP. A harness whose resolver ignores RES_OPTIONS, such as a musl build, cannot look names up, so it does not start. Wrappers are judged by their first file, a script by its interpreter, so one that starts a musl program still starts, then cannot look names up.",
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

/** The option an owner has until they pick another: the engine's `LinuxSockets` default. */
export const LINUX_SOCKETS_DEFAULT: LinuxSockets = "tcp_only";

/** The Linux call supervisor options in the order Settings lists them, each with its title and
 * the plain sentence naming what it keeps and what it leaves open, as the TUI says them. */
export const LINUX_SUPERVISOR: { value: LinuxSupervisor; title: string; detail: string }[] = [
  {
    value: "on",
    title: "On (recommended)",
    detail: "CrazyAgent checks some calls agents make and makes them for them, so ports on this computer, git repositories and signals stay within what you allow. Run as root, it starts none.",
  },
  {
    value: "off",
    title: "Off",
    detail: "Agents start as before: a port they may reach on this computer is open to them on every host, and they can make a git repository or, on older Linux, signal your programs.",
  },
];

/** The supervisor option an owner has until they pick another: the engine's `LinuxSupervisor` default. */
export const LINUX_SUPERVISOR_DEFAULT: LinuxSupervisor = "on";

/** The strict Linux options in the order Settings lists them, each with its title and the plain
 * sentence naming what starts under it, as the TUI says them (DEC-185). */
export const LINUX_STRICT: { value: LinuxStrict; title: string; detail: string }[] = [
  {
    value: "off",
    title: "Off",
    detail: "Agents start with what this Linux holds, and the lines under Agent access say what it cannot hold for them.",
  },
  {
    value: "on",
    title: "On",
    detail: "An agent on Linux starts only with a process space of its own, the call supervisor running and its network limits held. Otherwise the master, a plugin check or a context-mode check does not start, and says why.",
  },
];

/** The strict option an owner has until they pick another: the engine's `LinuxStrict` default. */
export const LINUX_STRICT_DEFAULT: LinuxStrict = "off";

/** The settings Save sends: each Linux choice as picked, else as stored. Inputs: the stored
 * settings and the picked sockets, supervisor and strict options, each null until picked.
 * Output: the whole settings. */
export function sandboxDraft(stored: SandboxSettings, sockets: LinuxSockets | null, calls: LinuxSupervisor | null, strict: LinuxStrict | null): SandboxSettings {
  return {
    ...stored,
    linux_sockets: sockets ?? stored.linux_sockets,
    linux_supervisor: calls ?? stored.linux_supervisor,
    linux_strict: strict ?? stored.linux_strict,
  };
}

const SANDBOX_TOP = <div className="settings-top"><div>
  <h2>Sandbox</h2>
  <p>What agents can reach while they work.</p>
</div></div>;

/** Settings, Sandbox as a view of its state: the title in every state, then the Linux
 * limited-network sockets, call supervisor and strict choices, each marked when it is away from
 * the default, with Save changes, enabled only after a change. Inputs: the settings read, the
 * picked options, a saving flag, the last problem and the handlers. Output: the tab. */
export function SandboxSettingsView({ settings, picked, pickedSupervisor, pickedStrict, saving, problem, onPick, onPickSupervisor, onPickStrict, onSave, onRetry }: {
  settings: Loaded<SandboxSettings>;
  picked: LinuxSockets | null;
  pickedSupervisor: LinuxSupervisor | null;
  pickedStrict: LinuxStrict | null;
  saving: boolean;
  problem: string;
  onPick: (value: LinuxSockets) => void;
  onPickSupervisor: (value: LinuxSupervisor) => void;
  onPickStrict: (value: LinuxStrict) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <>{SANDBOX_TOP}<Skeleton shape="line" count={4} /></>;
  if (settings.kind === "not_ready") return <>{SANDBOX_TOP}<NotReady what="The sandbox settings" /></>;
  if (settings.kind === "error") return <>{SANDBOX_TOP}<LoadError message={`Couldn't load the sandbox settings. ${settings.message}`} onRetry={onRetry} /></>;
  const draft = sandboxDraft(settings.value, picked, pickedSupervisor, pickedStrict);
  const chosen = draft.linux_sockets;
  const calls = draft.linux_supervisor;
  const strict = draft.linux_strict;
  const changed = chosen !== settings.value.linux_sockets || calls !== settings.value.linux_supervisor || strict !== settings.value.linux_strict;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <>
    {SANDBOX_TOP}
    <form className="pj set-groups" aria-label="Linux sandbox settings" noValidate onSubmit={submit}>
      <SettingSection title="Linux">
        <SettingLine wide label="Linux limited-network sockets" changed={chosen !== LINUX_SOCKETS_DEFAULT}
          help="How an agent limited to its model reaches the network when CrazyAgent runs on Linux. Landlock limits TCP by port only.">
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">Linux limited-network sockets</legend>
            {LINUX_SOCKETS.map((option) => <RadioCard key={option.value} name="linux-sockets" value={option.value} checked={chosen === option.value}
              onChecked={(value) => onPick(value as LinuxSockets)} title={option.title} details={[option.detail]} />)}
          </fieldset>
        </SettingLine>
        <SettingLine wide label="Linux call supervisor" changed={calls !== LINUX_SUPERVISOR_DEFAULT}
          help="Whether CrazyAgent checks the calls Landlock can't limit by itself when agents make them, when CrazyAgent runs on Linux.">
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">Linux call supervisor</legend>
            {LINUX_SUPERVISOR.map((option) => <RadioCard key={option.value} name="linux-supervisor" value={option.value} checked={calls === option.value}
              onChecked={(value) => onPickSupervisor(value as LinuxSupervisor)} title={option.title} details={[option.detail]} />)}
          </fieldset>
        </SettingLine>
        <SettingLine wide label="Refuse agents without the full Linux sandbox" changed={strict !== LINUX_STRICT_DEFAULT}
          help="Whether CrazyAgent starts an agent on Linux when this computer cannot hold everything its sandbox can, when CrazyAgent runs on Linux.">
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">Refuse agents without the full Linux sandbox</legend>
            {LINUX_STRICT.map((option) => <RadioCard key={option.value} name="linux-strict" value={option.value} checked={strict === option.value}
              onChecked={(value) => onPickStrict(value as LinuxStrict)} title={option.title} details={[option.detail]} />)}
          </fieldset>
        </SettingLine>
        <div className="set-body">
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
        </div>
      </SettingSection>
    </form>
  </>;
}

/** Settings, Sandbox: reads the owner's sandbox settings and saves the picked Linux
 * limited-network sockets, call supervisor and strict options in the engine, all in one save. */
export function SandboxSettingsTab() {
  const session = useAppSession();
  const [settings, reload] = useLoad(getSandboxSettings);
  const [picked, setPicked] = useState<LinuxSockets | null>(null);
  const [pickedSupervisor, setPickedSupervisor] = useState<LinuxSupervisor | null>(null);
  const [pickedStrict, setPickedStrict] = useState<LinuxStrict | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setPicked(null); setPickedSupervisor(null); setPickedStrict(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok" || (picked === null && pickedSupervisor === null && pickedStrict === null)) return;
    setSaving(true);
    setProblem("");
    const result = await saveSandboxSettings(sandboxDraft(settings.value, picked, pickedSupervisor, pickedStrict));
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Sandbox setting saved.");
    reload();
  }
  return <div className="stack">
    <SandboxSettingsView settings={settings} picked={picked} pickedSupervisor={pickedSupervisor} pickedStrict={pickedStrict} saving={saving} problem={problem}
      onPick={(value) => { setPicked(value); setProblem(""); }} onPickSupervisor={(value) => { setPickedSupervisor(value); setProblem(""); }}
      onPickStrict={(value) => { setPickedStrict(value); setProblem(""); }} onSave={() => void save()} onRetry={reload} />
  </div>;
}
