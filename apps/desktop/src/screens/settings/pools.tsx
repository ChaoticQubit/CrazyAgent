import { useEffect, useState, type ReactNode } from "react";
import { projectsUsing } from "../../api-projects";
import { accountRanking, accountSchedule, deletePool, listAccounts, poolSummaries, readyForAgents, savePool, POOL_MEMBER_MAX, type AccountPlan, type AccountSchedule, type AccountView, type PoolBase, type PoolMember, type PoolRanking, type PoolSummary, type RankedAccount, type Result } from "../../api";
import { loadUsage, type AccountLimits } from "../../api-usage";
import { useAppSession, useLoad, type Loaded } from "../../state";
import {
  Button, CapacityMeter, ConfirmDialog, EmptyState, LoadError, Notice, NotReady, SearchField, Select, SettingLine, SettingSection, Skeleton,
  StatusBadge, TextField, WindowedList, navigate, Checkbox,
} from "../../ui";
import { offsetMinutes } from "../metrics/usage-model";
import { AccountChoice, planFor, planLine, poolStrategy } from "./account-choice";
import { andList } from "./account-drawer";
import { filterAccounts } from "./accounts";
import { takePoolDraft } from "./pool-draft";
import { HELD_HEAD, heldLine, isNear, orderLine, usageBars, whyLine } from "./pool-order";

export const POOL_FULL = "A pool can hold up to 15 accounts. Remove one to add another.";

/** How often the page reads the order and the limits again, in milliseconds. */
const REREAD_MS = 60_000;

/** Describes how many pool members are ready: "3 ready, 1 not signed in", or "No accounts".
 * Input: the members. Output: the line. */
export function poolReadiness(members: PoolMember[]): string {
  if (members.length === 0) return "No accounts";
  const waiting = members.filter((member) => member.status === "not_signed_in").length;
  const ready = members.length - waiting;
  return waiting ? `${ready} ready, ${waiting} not signed in` : `${ready} ready`;
}

/** Describes which projects use a pool. Input: the project count. Output: the line. */
export function poolUse(count: number): string {
  return count === 0 ? "Not used" : count === 1 ? "Used by 1 project" : `Used by ${count} projects`;
}

/** Adds or removes one account from a pool draft; a 16th account is refused.
 * Inputs: member ids, the account id, the cap. Output: the new members, or "full". */
export function toggleMember(members: string[], id: string, max = POOL_MEMBER_MAX): { kind: "ok"; members: string[] } | { kind: "full" } {
  if (members.includes(id)) return { kind: "ok", members: members.filter((member) => member !== id) };
  if (members.length >= max) return { kind: "full" };
  return { kind: "ok", members: [...members, id] };
}

/** The note under the counter when members are not signed in. Inputs: member ids and the
 * known statuses by id. Output: the note, or undefined when every member is ready. */
export function notSignedInNote(members: string[], status: Map<string, string>): string | undefined {
  const count = members.filter((id) => status.get(id) === "not_signed_in").length;
  if (count === 0) return undefined;
  return count === 1 ? "1 account isn't signed in. Agents skip it until you sign in again."
    : `${count} accounts aren't signed in. Agents skip them until you sign in again.`;
}

/** The delete confirm for a pool: deleting takes it off every project that uses it in the same
 * engine step. Inputs: the pool and the names of its projects (undefined when not read).
 * Output: the body, the button label and the line shown once deleted. */
export function deleteCopy(pool: PoolSummary, users: readonly string[] | undefined): { body: string; confirmLabel: string; done: string } {
  const count = users?.length || pool.project_count;
  if (count === 0) return { body: "Its accounts stay in your list. Only the pool goes away.", confirmLabel: "Delete pool", done: `${pool.name} deleted.` };
  const named = users?.length ? andList(users) : `${count} ${count === 1 ? "project" : "projects"}`;
  return {
    body: `Used by ${named}. ${count === 1 ? "It" : "They"} will have no pool until you pick one. Its accounts stay in your list.`,
    confirmLabel: "Unassign and delete",
    done: `${pool.name} deleted. Its projects have no pool now. Pick a pool to start in each project's Settings.`,
  };
}

/** The accounts newly added to a pool that agents can use only after the engine sets them
 * up: saved-key and URL accounts (subscriptions sign in instead). Inputs: the members before
 * and after saving, and the known accounts. Output: those accounts, in member order. */
export function setupTargets(previous: string[], members: string[], accounts: AccountView[]): AccountView[] {
  const known = new Map(accounts.map((account) => [account.id, account]));
  return members.filter((id) => !previous.includes(id)).map((id) => known.get(id))
    .filter((account): account is AccountView => account !== undefined && (account.method === "api_key" || account.method === "url"));
}

/** The members of a saved pool that agents use through a harness profile: saved-key, URL
 * and local accounts. Input: the pool. Output: their ids, in member order. */
export function keyedMembers(pool: PoolSummary): string[] {
  return pool.members.filter((member) => member.kind === "api_key" || member.kind === "url" || member.kind === "local").map((member) => member.id);
}

/** The note after saving a pool when some accounts are not ready for agents yet.
 * Input: the failed accounts with plain messages. Output: one sentence, or undefined. */
export function setupNote(failures: { name: string; message: string }[]): string | undefined {
  if (failures.length === 0) return undefined;
  const names = failures.map((failure) => failure.name);
  const list = names.length === 1 ? names[0] : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  return `${list} ${names.length === 1 ? "isn't" : "aren't"} ready for agents yet. ${failures[0]!.message}`;
}

/** Sets accounts up for agents one at a time. Inputs: the accounts and the session's result
 * recorder. Output: the accounts that are not ready, each with a plain message. */
export async function readyEach(targets: { id: string; name: string }[], note: <T>(result: Result<T>) => Result<T>): Promise<{ name: string; message: string }[]> {
  const failures: { name: string; message: string }[] = [];
  for (const target of targets) {
    const ready = note(await readyForAgents(target.id));
    if (ready.kind !== "ok") failures.push({ name: target.name, message: ready.kind === "not_ready" ? "Setting accounts up for agents isn't ready in this build." : ready.message });
  }
  return failures;
}

/** The setup step after any save that adds pool members, from the pool editor or Add to
 * pool: each newly added saved-key or URL account is set up for agents. Inputs: the members
 * before and after the save, the known accounts and the session's result recorder. Output:
 * the note naming the accounts that are not ready, or undefined when all are. */
export async function readyAdded(previous: string[], members: string[], accounts: AccountView[], note: <T>(result: Result<T>) => Result<T>): Promise<string | undefined> {
  return setupNote(await readyEach(setupTargets(previous, members, accounts), note));
}

/** One account's usage on its row. Inputs: the limits read, the account id and now. Output: its
 * bars, the line for an account with no reading, or nothing while the limits are not read. */
function RowUsage({ limits, id, nowMs }: { limits: Loaded<AccountLimits[]>; id: string; nowMs: number }) {
  if (limits.kind !== "ok") return null;
  const found = limits.value.find((row) => row.account === id);
  const bars = found ? usageBars(found, nowMs) : [];
  if (bars.length === 0) return <div className="pool-limit-none">No usage read yet</div>;
  return <div className="pool-limits">{bars.map((bar) => <div key={bar.name} className={bar.tone === "ok" ? "pool-limit" : `pool-limit ${bar.tone}`}>
    <span>{bar.name}</span><span className="pool-bar" aria-hidden="true"><i style={{ width: `${bar.percent}%` }} /></span><span>{bar.text}</span>
  </div>)}</div>;
}

/** One account in the selected pool's order: where it stands, who it is, why it stands there and
 * its usage. Inputs: the ranked account, its place (null for a held one), what is known of it,
 * its plan when read, the limits read and now. Output: the row. */
function OrderRow({ item, at, account, member, plan, limits, nowMs }: {
  item: RankedAccount;
  at: number | null;
  account: AccountView | undefined;
  member: PoolMember | undefined;
  plan: AccountPlan | undefined;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
}) {
  const id = item.connection;
  const name = account?.name ?? member?.provider ?? id;
  const harness = account?.provider_name ?? member?.provider ?? "";
  const status = account?.status_label ?? member?.status_label ?? "";
  const near = limits.kind === "ok" && limits.value.some((row) => row.account === id && isNear(row, nowMs));
  const why = item.standing.kind === "next" ? whyLine(item.standing.why, nowMs) : heldLine(item.standing, nowMs);
  const signIn = item.standing.kind === "not_signed_in";
  return <li className={at === null ? "pool-row held" : "pool-row"}>
    <span className="pool-pos"><span aria-hidden="true">{at === null ? "-" : at + 1}</span><span className="visually-hidden">{at === null ? "Held" : `Position ${at + 1}`}</span></span>
    <div className="pool-who">
      <div className="pool-who-top"><span className="pool-name ellipsis" title={name}>{name}</span>{status && <StatusBadge word={status} />}{near && <StatusBadge word="Near its limit" tone="warn" />}</div>
      <div className="pool-plan">{plan ? `${harness} \u00b7 ${planLine(plan)}` : harness}</div>
      <div className="pool-why">{why}{signIn && <> <a href={`#/settings/accounts?action=sign_in&account=${encodeURIComponent(id)}`} aria-label={`Sign in ${name}`}>Sign in</a></>}</div>
    </div>
    <div className="pool-right"><RowUsage limits={limits} id={id} nowMs={nowMs} /></div>
  </li>;
}

/** The selected pool's accounts in the engine's order, then the held ones; the page never sorts
 * them. Inputs: the pool, the four reads, now and the retry action. Output: the rows, or the
 * state of the order read. */
function PoolOrder({ pool, ranking, schedule, accounts, limits, nowMs, onRetry }: {
  pool: PoolSummary;
  ranking: Loaded<PoolRanking[]>;
  schedule: Loaded<AccountSchedule>;
  accounts: Loaded<AccountView[]>;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
  onRetry: () => void;
}) {
  if (ranking.kind === "loading") return <div className="set-body"><Skeleton shape="row" count={4} /></div>;
  if (ranking.kind === "not_ready") return <div className="set-body"><NotReady what="Account order" compact /></div>;
  if (ranking.kind === "error") return <div className="set-body"><LoadError message="Couldn't load the account order." onRetry={onRetry} /></div>;
  const found = ranking.value.find((item) => item.pool === pool.id);
  const rows = found?.accounts ?? [];
  const ready = rows.filter((item) => item.standing.kind === "next");
  const held = rows.filter((item) => item.standing.kind !== "next");
  const byId = new Map(accounts.kind === "ok" ? accounts.value.map((account) => [account.id, account]) : []);
  const members = new Map(pool.members.map((member) => [member.id, member]));
  const row = (item: RankedAccount, at: number | null) => <OrderRow key={item.connection} item={item} at={at} account={byId.get(item.connection)} member={members.get(item.connection)}
    plan={schedule.kind === "ok" ? planFor(schedule.value, item.connection) : undefined} limits={limits} nowMs={nowMs} />;
  return <>
    {found && schedule.kind === "ok" && <div className="pool-order">
      <span className="pool-order-head">Account order</span>
      <p>{orderLine(found.strategy, poolStrategy(schedule.value, pool.id) !== null, found.own_order_projects)}</p>
    </div>}
    {limits.kind === "error" && <div className="set-body"><Notice tone="warn">Couldn't read usage limits.</Notice></div>}
    {rows.length === 0 && <p className="pool-empty">{pool.members.length === 0 ? "No accounts yet. Use Edit pool to put one in this pool." : "No accounts to rank yet."}</p>}
    {rows.length > 0 && ready.length === 0 && <p className="pool-empty">No account can take work now.</p>}
    {ready.length > 0 && <ol className="pool-rows">{ready.map((item, at) => row(item, at))}</ol>}
    {held.length > 0 && <><p className="pool-held-head">{HELD_HEAD}</p><ul className="pool-rows">{held.map((item) => row(item, null))}</ul></>}
  </>;
}

/** Settings, Pools as a pure view: the pools on the left, and on the right the selected pool with
 * its accounts in their real order. Inputs: the five reads, now, the selected pool's id (the first
 * pool when absent or gone), the select, new, edit, delete, retry and set-up actions and the pool
 * being set up. Output: the screen, or the state of the pools read. */
export function PoolsScreenView({ pools, accounts, schedule, ranking, limits, nowMs, selected, onSelect, onNew, onEdit, onDelete, onRetry, onRetryOrder, onPrepare, preparing = null }: {
  pools: Loaded<PoolSummary[]>;
  accounts: Loaded<AccountView[]>;
  schedule: Loaded<AccountSchedule>;
  ranking: Loaded<PoolRanking[]>;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
  selected: string | undefined;
  onSelect: (id: string) => void;
  onNew: () => void;
  onEdit: (pool: PoolSummary) => void;
  onDelete: (pool: PoolSummary) => void;
  onRetry: () => void;
  onRetryOrder: () => void;
  onPrepare?: (pool: PoolSummary) => void;
  preparing?: string | null;
}) {
  if (pools.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (pools.kind === "not_ready") return <NotReady what="Pools" />;
  if (pools.kind === "error") return <LoadError message="Couldn't load pools." onRetry={onRetry} />;
  if (pools.value.length === 0) return <EmptyState icon="layers" title="No pools yet"
    action={<Button variant="primary" icon="plus" onClick={onNew}>New pool</Button>}>
    A pool groups up to 15 accounts for your projects to use.
  </EmptyState>;
  const pool = pools.value.find((item) => item.id === selected) ?? pools.value[0]!;
  const note = notSignedInNote(pool.members.map((member) => member.id), new Map(pool.members.map((member) => [member.id, member.status])));
  return <div className="pool-screen">
    <SettingSection title="Your pools" actions={<Button size="small" icon="plus" onClick={onNew}>New pool</Button>}>
      <div className="pool-items">{pools.value.map((item) => <button key={item.id} type="button" className={item.id === pool.id ? "pool-item on" : "pool-item"} aria-current={item.id === pool.id ? "true" : undefined} onClick={() => onSelect(item.id)}>
        <span className="pool-item-name"><span className="pool-item-label">{item.name}</span><span className="pool-item-count">{item.members.length} of {POOL_MEMBER_MAX}</span></span>
        <span className="pool-item-about">{`${poolReadiness(item.members)}. ${poolUse(item.project_count)}.`}</span>
      </button>)}</div>
    </SettingSection>
    <div className="pool-detail">
      <SettingSection title={pool.name} about={`${poolUse(pool.project_count)}. ${poolReadiness(pool.members)}. Shown on each project's pool picker.`} actions={<div className="pool-actions">
        {onPrepare && keyedMembers(pool).length > 0 && <Button icon="refresh-cw" aria-label={`Get ready for agents: ${pool.name}`} pending={preparing === pool.id} pendingLabel="Getting ready..." disabled={preparing !== null} onClick={() => onPrepare(pool)}>Get ready for agents</Button>}
        <Button aria-label={`Edit pool ${pool.name}`} onClick={() => onEdit(pool)}>Edit pool</Button>
        <Button variant="quiet" aria-label={`Delete pool ${pool.name}`} onClick={() => onDelete(pool)}>Delete pool</Button>
      </div>}>
        <div className="pool-cap"><CapacityMeter count={pool.members.length} /></div>
        {note && <div className="set-body"><Notice tone="warn">{note}</Notice></div>}
        <PoolOrder pool={pool} ranking={ranking} schedule={schedule} accounts={accounts} limits={limits} nowMs={nowMs} onRetry={onRetryOrder} />
      </SettingSection>
    </div>
  </div>;
}

/** The body of All accounts for each state of the read: loading, not ready, failed, no accounts
 * at all, nothing shown, or the windowed lines. Inputs: the read, the accounts shown, whether
 * Selected only has nothing to show and the line for one account. Output: the list. */
function AccountList({ accounts, shown, noneSelected, line }: {
  accounts: Loaded<AccountView[]>;
  shown: AccountView[];
  noneSelected: boolean;
  line: (account: AccountView) => ReactNode;
}) {
  if (accounts.kind === "loading") return <div className="set-body"><Skeleton shape="row" count={6} /></div>;
  if (accounts.kind === "not_ready") return <div className="set-body"><NotReady what="Accounts" compact /></div>;
  if (accounts.kind === "error") return <div className="set-body"><Notice tone="danger">Couldn't load accounts.</Notice></div>;
  if (accounts.value.length === 0) return <div className="set-body"><EmptyState compact icon="key-round" title="No accounts yet"
    action={<a className="btn" href="#/settings/accounts?action=add">Add account</a>}>Add an account first, then put it in a pool.</EmptyState></div>;
  if (shown.length === 0) return <p className="set-body secondary">{noneSelected ? "No accounts selected yet." : "No accounts match."}</p>;
  return <WindowedList label="All accounts" count={shown.length} viewport={440} renderRow={(index) => line(shown[index]!)} />;
}

/** The members of the pool being edited. Inputs: the member ids, the accounts by id, the saved
 * members by id and the remove action. Output: the list. */
function MemberLines({ members, byId, known, onRemove }: {
  members: string[];
  byId: Map<string, AccountView>;
  known: Map<string, PoolMember>;
  onRemove: (id: string) => void;
}) {
  return <ul className="plain-list">{members.map((id) => {
    const account = byId.get(id);
    const member = known.get(id);
    const label = account?.name ?? member?.provider ?? "Account";
    return <li className="member" key={id}>
      <span className="grow pick-words"><span className="ellipsis">{label}</span>
        {account && <span className="small secondary ellipsis">{account.provider_name}</span>}</span>
      <StatusBadge word={account?.status_label ?? member?.status_label ?? ""} />
      <Button variant="quiet" size="small" aria-label={`Remove ${label}`} onClick={() => onRemove(id)}>Remove</Button>
    </li>;
  })}</ul>;
}

/** The accounts the pool editor's left list shows: the search and provider filters, then only
 * the members when "Selected only" is on. Inputs: the accounts, the filters, the members and
 * the switch. Output: the accounts to list. */
export function editorAccounts(accounts: AccountView[], search: string, provider: string, members: readonly string[], selectedOnly: boolean): AccountView[] {
  const shown = filterAccounts(accounts, search, provider, "");
  return selectedOnly ? shown.filter((account) => members.includes(account.id)) : shown;
}

/** The pool editor as a view: name, every account as a windowed checkbox list, the members
 * with the 15-cell counter, and Save pool. Inputs: the pool being edited (absent for a new
 * one), the accounts read, the members a new pool starts with, and the save and cancel actions. */
export function PoolEditorView({ pool, accounts, initialMembers = [], onSave, onCancel }: {
  pool: PoolSummary | undefined;
  accounts: Loaded<AccountView[]>;
  initialMembers?: string[];
  onSave: (name: string, members: string[]) => Promise<Result<void>>;
  onCancel: () => void;
}) {
  const [name, setName] = useState(pool?.name ?? "");
  const opened = pool?.members.map((member) => member.id) ?? initialMembers.slice(0, POOL_MEMBER_MAX);
  const [members, setMembers] = useState<string[]>(opened);
  const [selectedOnly, setSelectedOnly] = useState(false);
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("");
  const [blocked, setBlocked] = useState<string>();
  const [nameError, setNameError] = useState<string>();
  const [saveError, setSaveError] = useState<string>();
  const [saving, setSaving] = useState(false);
  const all = accounts.kind === "ok" ? accounts.value : [];
  const byId = new Map(all.map((account) => [account.id, account]));
  const known = new Map(pool?.members.map((member) => [member.id, member]) ?? []);
  const status = new Map<string, string>([...pool?.members.map((member) => [member.id, member.status] as [string, string]) ?? [],
    ...all.map((account) => [account.id, account.status] as [string, string])]);
  const full = members.length >= POOL_MEMBER_MAX;
  const shown = editorAccounts(all, search, provider, members, selectedOnly);
  const providers = [...new Map(all.map((account) => [account.provider, account.provider_name])).entries()];
  const note = notSignedInNote(members, status);
  const membersChanged = members.length !== opened.length || members.some((id, index) => id !== opened[index]);

  const toggle = (id: string) => {
    const next = toggleMember(members, id);
    if (next.kind === "full") {
      setBlocked(id);
      document.getElementById("pool-full-notice")?.focus();
      return;
    }
    setBlocked(undefined);
    setMembers(next.members);
  };

  const save = async () => {
    if (!name.trim()) return setNameError("Enter a pool name.");
    setNameError(undefined);
    setSaving(true);
    const result = await onSave(name, members);
    setSaving(false);
    if (result.kind === "ok") return;
    if (result.kind === "error" && result.code.startsWith("pool_name")) return setNameError(result.message);
    setSaveError(result.kind === "not_ready" ? "Saving pools isn't ready yet in this build." : `Couldn't save this pool. ${result.message}`);
  };

  const accountLine = (account: AccountView) => {
    const checked = members.includes(account.id);
    const refused = blocked === account.id;
    return <label className={`pick${refused ? " blocked" : ""}`} title={full && !checked ? POOL_FULL : undefined}>
      <span className="pick-words"><span className="ellipsis">{account.name}</span>
        <span className="small secondary ellipsis">{refused ? POOL_FULL : `${account.provider_name}. ${account.status_label}.`}</span></span>
      <input type="checkbox" checked={checked} aria-disabled={(full && !checked) || undefined} onChange={() => toggle(account.id)} />
    </label>;
  };

  return <form className="stack" onSubmit={(event) => { event.preventDefault(); void save(); }}>
    <div className="settings-top">
      <div><h2>{pool ? `Edit ${pool.name}` : "New pool"}</h2><p>Pick the accounts this pool can use. A pool holds up to {POOL_MEMBER_MAX} accounts.</p></div>
      <Button icon="arrow-left" onClick={onCancel}>Back to pools</Button>
    </div>
    <SettingSection title="Details">
      <SettingLine label="Pool name" help="Shown on each project's pool picker." htmlFor="pool-name" changed={name !== (pool?.name ?? "")}>
        <TextField id="pool-name" label="Pool name" labelOutside value={name} onValue={setName} maxLength={60} error={nameError} />
      </SettingLine>
    </SettingSection>
    <div className="settings-two">
      <SettingSection title="All accounts">
        <div className="set-bar">
          <SearchField label="Search accounts" value={search} onValue={setSearch} />
          <Select id="pool-provider" label="Provider" hideLabel value={provider} onValue={setProvider}
            options={[{ value: "", label: "All providers" }, ...providers.map(([value, label]) => ({ value, label }))]} />
          <Checkbox label="Selected only" checked={selectedOnly} onChecked={setSelectedOnly} />
        </div>
        <AccountList accounts={accounts} shown={shown} noneSelected={selectedOnly && members.length === 0} line={accountLine} />
      </SettingSection>
      <SettingSection title="In this pool" changed={membersChanged}>
        <div className="set-body">
          <CapacityMeter count={members.length} />
          {full && <Notice id="pool-full-notice" alert={blocked !== undefined}>{POOL_FULL}</Notice>}
          {note && <Notice tone="info">{note}</Notice>}
          {members.length === 0 && <p className="secondary">No accounts yet. Check accounts on the left to add them.</p>}
        </div>
        {members.length > 0 && <MemberLines members={members} byId={byId} known={known} onRemove={toggle} />}
      </SettingSection>
    </div>
    {saveError && <Notice tone="danger" alert>{saveError}</Notice>}
    <div className="row">
      <Button type="submit" variant="primary" icon="check" pending={saving} pendingLabel="Saving...">Save pool</Button>
      <Button onClick={onCancel}>Cancel</Button>
    </div>
  </form>;
}

/** Settings, Pools: the one screen, the pool editor and Delete pool; the order and the limits are
 * read again once a minute. Input: whether a new pool editor opens on arrival. */
export function Pools({ creating }: { creating: boolean }) {
  const session = useAppSession();
  const [pools, reload] = useLoad(poolSummaries, [], "pool-summaries");
  const [accounts] = useLoad(listAccounts, [], "accounts");
  const [schedule, rereadSchedule] = useLoad(accountSchedule, [], "account-schedule");
  const [ranking, rerank] = useLoad(accountRanking, [], "account-ranking");
  const [usage, rereadUsage] = useLoad(() => loadUsage(null, offsetMinutes(new Date()), null), [], "pool-usage");
  const [selected, setSelected] = useState<string>();
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [editing, setEditing] = useState<{ id?: string; opened?: PoolBase } | undefined>(creating ? {} : undefined);
  const [draft, setDraft] = useState<string[]>([]);
  const [deleting, setDeleting] = useState<PoolSummary>();
  const [users, setUsers] = useState<string[]>();
  const [removing, setRemoving] = useState(false);
  const [preparing, setPreparing] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<string>();
  useEffect(() => {
    if (!creating) return;
    setEditing({});
    const taken = takePoolDraft();
    if (taken.length) setDraft(taken);
  }, [creating]);
  useEffect(() => {
    const timer = setInterval(() => { setNowMs(Date.now()); rerank(); rereadUsage(); }, REREAD_MS);
    return () => clearInterval(timer);
  }, []);
  const limits: Loaded<AccountLimits[]> = usage.kind === "ok" ? { kind: "ok", value: usage.value.limits } : usage;
  const reread = () => { reload(); rerank(); };

  const prepareAll = async (pool: PoolSummary) => {
    setPreparing(pool.id);
    setRefusal(undefined);
    const names = new Map(accounts.kind === "ok" ? accounts.value.map((account) => [account.id, account.name]) : []);
    const ids = keyedMembers(pool);
    const failures = await readyEach(ids.map((id) => ({ id, name: names.get(id) ?? "An account" })), session.noteResult);
    setPreparing(null);
    setRefusal(setupNote(failures));
    if (failures.length === 0) session.showToast(`${pool.name} is ready for agents.`);
  };

  const leave = () => {
    setEditing(undefined);
    if (creating) navigate({ name: "settings", tab: "pools" }, true);
  };

  if (editing) {
    const pool = editing.id && pools.kind === "ok" ? pools.value.find((item) => item.id === editing.id) : undefined;
    return <PoolEditorView key={editing.id ?? `new-${draft.join(",")}`} pool={pool} accounts={accounts} initialMembers={editing.id ? [] : draft} onCancel={() => { setDraft([]); leave(); }} onSave={async (name, members) => {
      const saved = session.noteResult(await savePool(editing.id, name, members, editing.opened));
      if (saved.kind !== "ok") return saved;
      setRefusal(await readyAdded(pool?.members.map((member) => member.id) ?? [], members, accounts.kind === "ok" ? accounts.value : [], session.noteResult));
      session.showToast(`${name.trim()} saved.`);
      reread();
      setDraft([]);
      leave();
      return saved;
    }} />;
  }

  const remove = async () => {
    if (!deleting) return;
    setRemoving(true);
    const result = session.noteResult(await deletePool(deleting.id));
    setRemoving(false);
    const pool = deleting;
    setDeleting(undefined);
    if (result.kind === "ok") { session.showToast(deleteCopy(pool, users).done); reread(); }
    else setRefusal(result.kind === "not_ready" ? "Deleting pools isn't ready yet in this build." : result.message);
  };

  const askDelete = async (pool: PoolSummary) => {
    setRefusal(undefined);
    setUsers(undefined);
    setDeleting(pool);
    if (pool.project_count === 0) return;
    const names = await projectsUsing(pool.id);
    if (names.kind === "ok") setUsers(names.value);
  };

  const copy = deleting && deleteCopy(deleting, users);

  return <div className="stack">
    <div className="settings-top">
      <div><h2>Pools</h2><p>A pool is a group of accounts for a project. When one account runs out, agents move to the next one in the pool.</p></div>
    </div>
    {refusal && <Notice tone="danger" alert><span className="grow">{refusal}</span></Notice>}
    <PoolsScreenView pools={pools} accounts={accounts} schedule={schedule} ranking={ranking} limits={limits} nowMs={nowMs} selected={selected} onSelect={setSelected} onRetryOrder={rerank}
      onNew={() => setEditing({})} onEdit={(pool) => setEditing({ id: pool.id, opened: { name: pool.name, members: pool.members.map((member) => member.id) } })}
      onDelete={(pool) => void askDelete(pool)} onRetry={reload} onPrepare={(pool) => void prepareAll(pool)} preparing={preparing} />
    {pools.kind === "ok" && pools.value.length > 0 && <AccountChoice pools={pools} accounts={accounts} schedule={schedule} ranking={ranking} reload={rereadSchedule} rerank={rerank} />}
    <ConfirmDialog open={Boolean(deleting)} title={`Delete ${deleting?.name ?? "pool"}?`} confirmLabel={copy?.confirmLabel ?? "Delete pool"} pending={removing}
      onCancel={() => setDeleting(undefined)} onConfirm={() => void remove()}>
      {copy?.body}
    </ConfirmDialog>
  </div>;
}

