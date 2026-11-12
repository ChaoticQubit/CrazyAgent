import { useContext, useEffect, useState } from "react";
import { checkUnconfirmed, listAccounts, nextGeneration, poolSummaries, receiptResult, type AccountView, type PoolSummary } from "../../api";
import { createDemo, loadDemo, type DemoView } from "../../api-demo";
import { needsReconcile, receiptState, settledReceipt } from "../../api-projects";
import { SessionContext, useLoad, type Loaded } from "../../state";
import { Button, Icon, LoadError, NotReady, Notice, PageHeader, Panel, RadioCard, Skeleton, formatRoute } from "../../ui";
import { DEMO_POLL_MS, demoEntry, demoPoolChoices, pickable, refusalCopy, type DemoEntry, type DemoPoolChoice } from "./model";
import { DEMO_ABOUT, DEMO_COST_NOTE, DEMO_CREATE, DEMO_ENTRY, DEMO_EXISTS, DEMO_NO_POOL, DEMO_NO_POOL_ACTION, DEMO_PICK } from "./words";
import "./demo.css";

const UNCONFIRMED = "We couldn't confirm the demo was made. Check your projects before trying again.";

/** Reads the demo, and reads it again every few seconds while some step is still open, so the
 * entry, the page and the guide follow the engine. Output: the latest read and a reload. */
export function useDemo(): [Loaded<DemoView>, () => void] {
  const [view, reload] = useLoad(loadDemo, [], "project-demo");
  const open = view.kind === "ok" && view.value.project_id !== null && !view.value.steps.change_set_accepted;
  useEffect(() => {
    if (!open) return;
    const timer = window.setTimeout(reload, DEMO_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [open, view]);
  return [view, reload];
}

/** The Home entry: its title, its two sentences and one action, Try the demo before the demo
 * exists and Open the demo once it does. Input: the entry, null until the demo read is good.
 * Output: the entry, or nothing while it cannot say which action is right. */
export function DemoEntryView({ entry }: { entry: DemoEntry | null }) {
  if (!entry) return null;
  return <Panel title={DEMO_ENTRY.title} className="demo-entry">
    <p className="secondary">{DEMO_ENTRY.body}</p>
    <span><a className={`btn${entry.action === "start" ? " primary" : ""} small`} href={entry.href}>
      <Icon name={entry.action === "start" ? "sparkles" : "arrow-right"} size={15} />{entry.label}</a></span>
  </Panel>;
}

/** The Home entry, reading the demo itself so its mount is one line. Output: the entry. */
export function DemoEntryPanel() {
  const [view] = useDemo();
  return <DemoEntryView entry={demoEntry(view)} />;
}

function PoolList({ choices, picked, pending, onPick }: { choices: DemoPoolChoice[]; picked: string | null; pending: boolean; onPick: (id: string) => void }) {
  return <fieldset className="demo-pools" disabled={pending}>
    <legend>{DEMO_PICK}</legend>
    {choices.map((choice) => <RadioCard key={choice.id} name="demo-pool" value={choice.id} checked={picked === choice.id && choice.reason === null}
      disabled={choice.reason !== null} onChecked={onPick} title={choice.name}
      details={[choice.reason ?? (choice.ready.length > 0 ? `Ready: ${choice.ready.join(", ")}` : "")].filter(Boolean)} />)}
  </fieldset>;
}

function DemoBody({ view, choices, picked, pending, error, onPick, onCreate, onRetry }: DemoPageProps) {
  if (view.kind === "loading") return <Skeleton shape="card" count={2} />;
  if (view.kind === "not_ready") return <NotReady what="The demo" compact />;
  if (view.kind === "error") return <LoadError message="Couldn't read the demo." onRetry={onRetry} />;
  const id = view.value.project_id;
  if (id) return <p className="demo-exists">{DEMO_EXISTS}{" "}
    <a className="btn primary small" href={formatRoute({ name: "project", id, tab: "conversation" })}><Icon name="arrow-right" size={15} />{DEMO_ENTRY.open}</a></p>;
  if (choices.kind === "loading") return <Skeleton shape="card" count={2} />;
  if (choices.kind === "not_ready") return <NotReady what="Pools" compact />;
  if (choices.kind === "error") return <LoadError message="Couldn't read your pools." onRetry={onRetry} />;
  if (choices.value.length === 0) return <p className="demo-no-pool">{DEMO_NO_POOL}{" "}
    <a className="btn small" href={formatRoute({ name: "settings", tab: "pools", action: "new" })}><Icon name="layers" size={15} />{DEMO_NO_POOL_ACTION}</a></p>;
  return <>
    <PoolList choices={choices.value} picked={picked} pending={pending} onPick={onPick} />
    {error && <Notice tone="danger" alert>{error}</Notice>}
    <div className="demo-actions">
      <Button variant="primary" icon="sparkles" pending={pending} pendingLabel="Creating the demo..." disabled={!pickable(choices.value, picked)} onClick={onCreate}>{DEMO_CREATE}</Button>
    </div>
  </>;
}

/** What the demo page shows, from its reads and the owner's pick. */
export interface DemoPageProps {
  view: Loaded<DemoView>;
  choices: Loaded<DemoPoolChoice[]>;
  picked: string | null;
  pending: boolean;
  error: string | null;
  onPick: (id: string) => void;
  onCreate: () => void;
  onRetry?: () => void;
}

/** The demo page as a view of its reads: the cost note first, what the demo makes, then the
 * pools with none picked, each with its ready accounts or the reason the demo cannot run on it,
 * and Create the demo, off until a pool that can run it is picked. With no pool it links to
 * Create a pool; once the demo exists it opens it instead. Input: the page's reads and state. */
export function DemoPageView(props: DemoPageProps) {
  return <>
    <PageHeader title={DEMO_ENTRY.title} />
    <div className="demo-page">
      <Notice tone="warn" icon="zap">{DEMO_COST_NOTE}</Notice>
      <p className="secondary">{DEMO_ABOUT}</p>
      <DemoBody {...props} onRetry={props.onRetry ?? (() => {})} />
    </div>
  </>;
}

function choicesOf(view: Loaded<DemoView>, pools: Loaded<PoolSummary[]>, accounts: Loaded<AccountView[]>): Loaded<DemoPoolChoice[]> {
  if (view.kind !== "ok") return view;
  if (pools.kind !== "ok") return pools;
  if (accounts.kind !== "ok") return accounts;
  return { kind: "ok", value: demoPoolChoices(view.value, pools.value, accounts.value) };
}

/** The demo page: reads the demo, the pools and the accounts, keeps the owner's pick and
 * sends Create the demo, then opens the new demo's Conversation, where the guide starts. A
 * refused or unconfirmed create keeps the pick and says why. Output: the page. */
export function DemoPage() {
  const session = useContext(SessionContext);
  const [view, reloadView] = useDemo();
  const [pools, reloadPools] = useLoad(poolSummaries, [], "pool-summaries");
  const [accounts, reloadAccounts] = useLoad(listAccounts, [], "accounts");
  const [picked, setPicked] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const choices = choicesOf(view, pools, accounts);

  async function create() {
    if (pending || picked === null || choices.kind !== "ok" || !pickable(choices.value, picked)) return;
    setPending(true);
    setError(null);
    const sent = await createDemo(picked, nextGeneration());
    if (sent.kind !== "ok") {
      setPending(false);
      setError(sent.kind === "not_ready" ? "Not ready yet: the demo isn't in this engine." : refusalCopy(sent));
      return;
    }
    let receipt = sent.value;
    if (needsReconcile(receipt)) {
      const settled = await checkUnconfirmed();
      session?.noteResult(settled.kind === "ok" ? receiptResult(settled.value) : settled);
      receipt = settledReceipt(receipt, settled);
    }
    const state = receiptState(receipt);
    const id = receipt.project_id;
    if (state.kind !== "applied" || !id) {
      setPending(false);
      setError(state.kind === "rejected" ? refusalCopy({ code: state.code }) : UNCONFIRMED);
      reloadView();
      return;
    }
    await session?.refreshProjects();
    window.location.hash = formatRoute({ name: "project", id, tab: "conversation" });
  }

  return <DemoPageView view={view} choices={choices} picked={picked} pending={pending} error={error}
    onPick={(id) => { setPicked(id); setError(null); }} onCreate={() => void create()}
    onRetry={() => { reloadView(); reloadPools(); reloadAccounts(); }} />;
}
