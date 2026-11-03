import { useState } from "react";
import { harnessCatalog, type HarnessAccountKind, type HarnessEntry } from "../../api";
import { useLoad, type Loaded } from "../../state";
import { Button, Icon, LoadError, NotReady, Panel, ProviderMark, Skeleton, StatusBadge, formatRoute } from "../../ui";
import { displayText } from "../conversation";
import { HarnessPage } from "./harness-page";

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

/** Where a harness row leads: a Ready or Limited control harness opens its settings page; a
 * harness that isn't installed has none and expands its details instead. Input: the entry.
 * Output: the page link, or null. */
export function harnessPageHref(entry: Pick<HarnessEntry, "id" | "status">): string | null {
  return entry.status.kind === "not_installed" ? null : formatRoute({ name: "settings", tab: "harnesses", harness: entry.id });
}

function HarnessDetails({ entry }: { entry: HarnessEntry }) {
  return <dl className="details-grid">
    <dt>Found at</dt><dd className="mono">{entry.executable ? homePath(entry.executable) : "Not found on this Mac"}</dd>
    <dt>Runs in the background with</dt><dd className="mono">{entry.headless}</dd>
    <dt>Model</dt><dd>{entry.model_selection ? "You can choose the model" : "Chosen inside the harness"}</dd>
  </dl>;
}

function RowCells({ entry }: { entry: HarnessEntry }) {
  return <>
    <span className="cell"><ProviderMark provider={entry.id} name={entry.name} /><span className="ellipsis">{entry.name}</span></span>
    <span><StatusBadge word={statusWord(entry)} /></span>
    <span className="secondary">{entry.tools.kind === "switchable" ? "Can turn on or off" : entry.tools.reason}</span>
    <span className="secondary">{entry.account_kinds.map((kind) => KIND_LABEL[kind]).join(", ") || "None"}</span>
  </>;
}

function HarnessRow({ entry }: { entry: HarnessEntry }) {
  const [open, setOpen] = useState(false);
  const href = harnessPageHref(entry);
  if (href !== null) {
    return <div><a className="harness-row" href={href} aria-label={`${entry.name} settings`}><RowCells entry={entry} /></a></div>;
  }
  return <div>
    <button type="button" className="harness-row" aria-expanded={open} onClick={() => setOpen(!open)}><RowCells entry={entry} /></button>
    {open && <HarnessDetails entry={entry} />}
  </div>;
}

/** The harness list as a pure view of the catalog read, with a Check again action.
 * Inputs: the catalog read, when it was checked and the recheck action. */
export function HarnessesView({ catalog, checkedAt, onCheck }: { catalog: Loaded<HarnessEntry[]>; checkedAt?: string; onCheck: () => void }) {
  return <div className="stack">
    <div className="row between">
      <p className="secondary">CrazyAgent finds harnesses installed on this Mac. It never installs them. Install one yourself, then choose Check again.</p>
      <span className="row">{checkedAt && <span className="muted small">Checked {checkedAt}</span>}
        <Button icon="refresh-cw" onClick={onCheck} disabled={catalog.kind === "loading"}>Check again</Button></span>
    </div>
    {catalog.kind === "loading" ? <Skeleton shape="row" count={8} />
      : catalog.kind === "not_ready" ? <NotReady what="The harness list" />
      : catalog.kind === "error" ? <LoadError message="Couldn't check harnesses." onRetry={onCheck} />
      : <Panel padded={false}>
        <div className="harness-row list-head" aria-hidden="true"><span>Harness</span><span>Status</span><span>Tools</span><span>Accounts it can use</span></div>
        {harnessGroups(catalog.value).map((group) => <section key={group.title} aria-label={group.title}>
          <h3 className="group-title"><Icon name={group.title === "Ready" ? "circle-check" : group.title === "Not installed" ? "circle-dashed" : "triangle-alert"} size={13} /> {group.title} ({group.entries.length})</h3>
          <div className="rows">{group.entries.map((entry) => <HarnessRow key={entry.id} entry={entry} />)}</div>
        </section>)}
      </Panel>}
  </div>;
}

/** One harness's settings page in Settings, Harnesses: a way back to the list, the harness with
 * its status and where it was found, then its app-wide settings. Inputs: the catalog read and
 * the harness id from the route. Output: the screen. */
export function HarnessScreenView({ catalog, id }: { catalog: Loaded<HarnessEntry[]>; id: string }) {
  const back = <a className="btn quiet" href={formatRoute({ name: "settings", tab: "harnesses" })}><Icon name="arrow-left" size={15} />All harnesses</a>;
  if (catalog.kind === "loading") return <div className="stack">{back}<Skeleton shape="row" count={6} /></div>;
  if (catalog.kind === "not_ready") return <div className="stack">{back}<NotReady what="The harness list" /></div>;
  if (catalog.kind === "error") return <div className="stack">{back}<p className="secondary" role="alert">Couldn't read the harness list. Go back and choose Check again.</p></div>;
  const entry = catalog.value.find((item) => item.id === id);
  if (entry === undefined) return <div className="stack">{back}<p className="secondary">CrazyAgent doesn't know the harness {displayText(id)}.</p></div>;
  return <div className="stack">
    {back}
    <div className="hs-head"><ProviderMark provider={entry.id} name={entry.name} large /><h2>{entry.name}</h2><StatusBadge word={statusWord(entry)} /></div>
    <HarnessDetails entry={entry} />
    <HarnessPage harness={entry.id} projectId={null} />
  </div>;
}

function HarnessScreen({ id }: { id: string }) {
  const [catalog] = useLoad(harnessCatalog, [], "harness-catalog");
  return <HarnessScreenView catalog={catalog} id={id} />;
}

/** Settings, Harnesses: the list, or one harness's settings page when the route names it.
 * Input: the harness id from the route, if any. */
export function Harnesses({ harness }: { harness?: string }) {
  return harness === undefined ? <HarnessList /> : <HarnessScreen key={harness} id={harness} />;
}

/** Settings, Harnesses list: has the engine look for harnesses again on every visit and on
 * Check again, so a harness installed a moment ago shows at once. */
function HarnessList() {
  const [catalog, reload] = useLoad(() => harnessCatalog(true), [], "harness-check");
  const [checkedAt, setCheckedAt] = useState<string>();
  return <HarnessesView catalog={catalog} checkedAt={checkedAt ?? (catalog.kind === "ok" ? "just now" : undefined)}
    onCheck={() => { reload(); setCheckedAt(`at ${new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`); }} />;
}
