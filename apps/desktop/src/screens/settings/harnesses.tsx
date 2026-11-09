import { useState } from "react";
import { harnessCatalog, type HarnessAccountKind, type HarnessEntry } from "../../api";
import { useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, ProviderMark, SettingSection, Skeleton, StatusBadge } from "../../ui";
import { SettingRow } from "../../ui/setting";
import "../project/projects.css";

const KIND_LABEL: Record<HarnessAccountKind, string> = {
  subscription: "Subscription",
  api_key: "API key",
  open_ai_compatible_url: "URL",
  local: "Local",
};

const GROUPS: { kind: HarnessEntry["status"]["kind"]; title: string }[] = [
  { kind: "ready", title: "Ready" },
  { kind: "limited", title: "Limited control" },
  { kind: "not_installed", title: "Not installed" },
];

/** Shows a path with the home folder as "~". Input: an absolute path. Output: the display path. */
export function homePath(path: string): string {
  return path.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
}

/** Groups the harness catalog Ready, then Limited control, then Not installed, keeping catalog
 * order inside each group. Input: the catalog. Output: non-empty groups with titles. */
export function harnessGroups(entries: HarnessEntry[]): { title: string; entries: HarnessEntry[] }[] {
  return GROUPS.map((group) => ({ title: group.title, entries: entries.filter((entry) => entry.status.kind === group.kind) }))
    .filter((group) => group.entries.length > 0);
}

function statusWord(entry: HarnessEntry): string {
  return entry.status.kind === "ready" ? "Ready" : entry.status.kind === "not_installed" ? "Not installed" : `Limited control: ${entry.status.reason}`;
}

function HarnessRow({ entry }: { entry: HarnessEntry }) {
  const [open, setOpen] = useState(false);
  return <>
    <SettingRow label={entry.name}>
      <button type="button" className="set-label" aria-expanded={open} onClick={() => setOpen(!open)}><ProviderMark provider={entry.id} name={entry.name} />{entry.name}</button>
      <span className="set-help">
        <span>Tools: {entry.tools.kind === "switchable" ? "Can turn on or off" : entry.tools.reason}</span>
        <span>Accounts it can use: {entry.account_kinds.map((kind) => KIND_LABEL[kind]).join(", ") || "None"}</span>
      </span>
      <div className="set-ctl set-line-ctl"><StatusBadge word={statusWord(entry)} /></div>
    </SettingRow>
    {open && <dl className="details-grid">
      <dt>Found at</dt><dd className="mono">{entry.executable ? homePath(entry.executable) : "Not found on this Mac"}</dd>
      <dt>Runs in the background with</dt><dd className="mono">{entry.headless}</dd>
      <dt>Model</dt><dd>{entry.model_selection ? "You can choose the model" : "Chosen inside the harness"}</dd>
    </dl>}
  </>;
}

/** The harness list as a pure view of the catalog read, with a Check again action.
 * Inputs: the catalog read, when it was checked and the recheck action. */
export function HarnessesView({ catalog, checkedAt, onCheck }: { catalog: Loaded<HarnessEntry[]>; checkedAt?: string; onCheck: () => void }) {
  return <div className="stack">
    <div className="settings-top">
      <div><h2>Harnesses</h2><p>CrazyAgent finds harnesses installed on this Mac. It never installs them. Install one yourself, then choose Check again.</p></div>
      <span className="row">{checkedAt && <span className="muted small">Checked {checkedAt}</span>}
        <Button icon="refresh-cw" onClick={onCheck} disabled={catalog.kind === "loading"}>Check again</Button></span>
    </div>
    {catalog.kind === "loading" ? <Skeleton shape="row" count={8} />
      : catalog.kind === "not_ready" ? <NotReady what="The harness list" />
      : catalog.kind === "error" ? <LoadError message="Couldn't check harnesses." onRetry={onCheck} />
      : harnessGroups(catalog.value).map((group) => <SettingSection key={group.title} title={`${group.title} (${group.entries.length})`}>
        {group.entries.map((entry) => <HarnessRow key={entry.id} entry={entry} />)}
      </SettingSection>)}
  </div>;
}

/** Settings, Harnesses: has the engine look for harnesses again on every visit and on Check
 * again, so a harness installed a moment ago shows at once. */
export function Harnesses() {
  const [catalog, reload] = useLoad(() => harnessCatalog(true), [], "harness-check");
  const [checkedAt, setCheckedAt] = useState<string>();
  return <HarnessesView catalog={catalog} checkedAt={checkedAt ?? (catalog.kind === "ok" ? "just now" : undefined)}
    onCheck={() => { reload(); setCheckedAt(`at ${new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`); }} />;
}
