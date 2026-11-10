import { useEffect, useRef, useState, type ReactNode } from "react";
import { projectsUsing } from "../../api-projects";
import { accountRanking, accountSchedule, createPool, deletePool, listAccounts, newPoolId, poolInput, poolSummaries, readyForAgents, savePool, POOL_MEMBER_MAX, type AccountPlan, type AccountSchedule, type AccountView, type PoolMember, type PoolRanking, type PoolSummary, type RankedAccount, type Result } from "../../api";
import { loadUsage, type AccountLimits } from "../../api-usage";
import { useAppSession, useHeld, useLoad, type Loaded } from "../../state";
import {
  Button, CapacityMeter, ConfirmDialog, EmptyState, Icon, LoadError, Notice, NotReady, SearchField, Select, SettingSection, Skeleton,
  StatusBadge, TextField, WindowedList, navigate,
} from "../../ui";
import { offsetMinutes } from "../metrics/usage-model";
import { planFor, planLine, poolStrategy } from "./account-choice";
import { andList } from "./account-drawer";
import { filterAccounts } from "./accounts";
import { takePoolDraft } from "./pool-draft";
import { AppOrder, KeepRowFocus, PoolOrderPicker, RowControls, SchedulePanel, applyHeld, usePoolChanges, type ApplyChange, type MadeChange } from "./pool-controls";
import { HELD_HEAD, heldLine, isNear, orderLine, usageBars, whyLine } from "./pool-order";

export const POOL_FULL = "A pool can hold up to 15 accounts. Remove one to add another.";

const DRAFT_EMPTY = "No accounts yet. This pool is created with its first account.";

/** What takes the focus when the control it was on left the rows. */
const ROWS_GONE = '[data-control="add-account"]';

/** What takes the focus when the control it was on left the add panel. */
const PANEL_GONE = ".pool-add input[type=search]";

/** How often the page reads the pools, the accounts, the order and the limits again, in
 * milliseconds, so a row's badge and why line and the left list never disagree for long. */
const REREAD_MS = 60_000;

/** The range the page asks usage for, in milliseconds. It reads only the limits, which the range
 * does not touch, so a short range spares a scan of all history on every read. */
const LIMITS_RANGE_MS = 3_600_000;

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

/** The most accounts a usage report holds, the engine's LIMITS_MAX: past it an account with no
 * row may still have a reading. */
const LIMITS_MAX = 64;

/** One account's usage on its row. Inputs: the limits read, the account id and now. Output: its
 * bars, the line for an account with no reading, or nothing while the limits are not read or
 * when a full report may have cut this account off. */
function RowUsage({ limits, id, nowMs }: { limits: Loaded<AccountLimits[]>; id: string; nowMs: number }) {
  if (limits.kind !== "ok") return null;
  const found = limits.value.find((row) => row.account === id);
  if (!found && limits.value.length >= LIMITS_MAX) return null;
  const bars = found ? usageBars(found, nowMs) : [];
  if (bars.length === 0) return <div className="pool-limit-none">No usage read yet</div>;
  return <div className="pool-limits">{bars.map((bar) => <div key={bar.name} className={bar.tone === "ok" ? "pool-limit" : `pool-limit ${bar.tone}`}>
    <span>{bar.name}</span><span className="pool-bar" aria-hidden="true"><i style={{ width: `${bar.percent}%` }} /></span><span>{bar.text}</span>
  </div>)}</div>;
}

/** One account in the selected pool's order: where it stands, who it is, why it stands there, its
 * pin and Schedule panel and its usage. Inputs: the ranked account, its place (null for a held
 * one), what is known of it, its plan when read, the limits read, now, whether its panel is open,
 * the toggle, the queue of changes and the removal. Output: the row. */
function OrderRow({ item, at, account, member, plan, limits, nowMs, open, onToggle, apply, onRemove }: {
  item: RankedAccount;
  at: number | null;
  account: AccountView | undefined;
  member: PoolMember | undefined;
  plan: AccountPlan | undefined;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
  open: boolean;
  onToggle: () => void;
  apply: ApplyChange;
  onRemove: (id: string, name: string) => void;
}) {
  const id = item.connection;
  const name = account?.name ?? member?.provider ?? id;
  const harness = account?.provider_name ?? member?.provider ?? "";
  const status = account?.status_label ?? member?.status_label ?? "";
  const near = limits.kind === "ok" && limits.value.some((row) => row.account === id && isNear(row, nowMs));
  const why = item.standing.kind === "next" ? whyLine(item.standing.why, nowMs) : heldLine(item.standing, nowMs);
  const signIn = item.standing.kind === "not_signed_in" || item.standing.kind === "sign_in_unconfirmed";
  return <li className={at === null ? "pool-row held" : "pool-row"}>
    <span className="pool-pos"><span aria-hidden="true">{at === null ? "-" : at + 1}</span><span className="visually-hidden">{at === null ? "Held" : `Position ${at + 1}`}</span></span>
    <div className="pool-who">
      <div className="pool-who-top"><span className="pool-name ellipsis" title={name}>{name}</span>{status && <StatusBadge word={status} />}{near && <StatusBadge word="Near its limit" tone="warn" />}</div>
      <div className="pool-plan">{plan ? `${harness} \u00b7 ${planLine(plan)}` : harness}</div>
      <div className="pool-why">{why}{signIn && <> <a href={`#/settings/accounts?action=sign_in&account=${encodeURIComponent(id)}`} aria-label={`Sign in ${name}`}>Sign in</a></>}</div>
    </div>
    <div className="pool-right">
      <div className="pool-ctl">
        {plan && <RowControls id={id} name={name} plan={plan} open={open} onToggle={onToggle} apply={apply} />}
        <Button variant="quiet" size="small" icon="x" iconOnly={`Remove ${name}`} data-control={`remove:${id}`} onClick={() => onRemove(id, name)} />
      </div>
      <RowUsage limits={limits} id={id} nowMs={nowMs} />
    </div>
    {plan && open && <SchedulePanel id={id} name={name} plan={plan} apply={apply} />}
  </li>;
}

/** The selected pool's account order and its accounts in the engine's order, then the held ones;
 * the page never sorts them. Inputs: the pool, the four reads, now, the retry action, the queue of
 * changes and the removal. Output: the rows, or the state of the order read. */
function PoolOrder({ pool, ranking, schedule, accounts, limits, nowMs, onRetry, apply, onRemove }: {
  pool: PoolSummary;
  ranking: Loaded<PoolRanking[]>;
  schedule: Loaded<AccountSchedule>;
  accounts: Loaded<AccountView[]>;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
  onRetry: () => void;
  apply: ApplyChange;
  onRemove: (id: string, name: string) => void;
}) {
  const [open, setOpen] = useState<string | null>(null);
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
    plan={schedule.kind === "ok" ? planFor(schedule.value, item.connection) : undefined} limits={limits} nowMs={nowMs}
    open={open === item.connection} onToggle={() => setOpen(open === item.connection ? null : item.connection)} apply={apply} onRemove={onRemove} />;
  return <>
    {found && schedule.kind === "ok" && <div className="pool-order">
      <div className="pool-order-pick"><span className="pool-order-head">Account order</span><PoolOrderPicker key={pool.id} pool={pool} schedule={schedule.value} apply={apply} /></div>
      <p>{orderLine(found.strategy, poolStrategy(schedule.value, pool.id) !== null, found.own_order_projects)}</p>
    </div>}
    {limits.kind === "error" && <div className="set-body"><Notice tone="warn">Couldn't read usage limits.</Notice></div>}
    {rows.length === 0 && <p className="pool-empty">{pool.members.length === 0 ? "No accounts yet. Use Add account to put one in this pool." : "No accounts to rank yet."}</p>}
    {rows.length > 0 && ready.length === 0 && <p className="pool-empty">No account can take work now.</p>}
    {ready.length > 0 && <ol role="list" className="pool-rows">{ready.map((item, at) => row(item, at))}</ol>}
    {held.length > 0 && <><p className="pool-held-head">{HELD_HEAD}</p><ul role="list" className="pool-rows">{held.map((item) => row(item, null))}</ul></>}
  </>;
}

/** Draws a started pool as the pool it will be. Input: the draft. Output: its summary. */
function draftSummary(draft: NewPool): PoolSummary {
  return { id: draft.id ?? "new-pool", name: draft.name, summary: "", members: draft.members, project_count: 0 };
}

/** One pool in the left list. Inputs: the pool, whether it is selected and the select action.
 * Output: its button. */
function PoolItem({ pool, on, onSelect }: { pool: PoolSummary; on: boolean; onSelect: () => void }) {
  return <button type="button" className={on ? "pool-item on" : "pool-item"} aria-current={on ? "true" : undefined} onClick={onSelect}>
    <span className="pool-item-name"><span className="pool-item-label">{pool.name}</span><span className="pool-item-count">{pool.members.length} of {POOL_MEMBER_MAX}</span></span>
    <span className="pool-item-about">{`${poolReadiness(pool.members)}. ${poolUse(pool.project_count)}.`}</span>
  </button>;
}

/** A saved pool's name, saved on Enter or on leaving the field; a name that fails its rule shows
 * it and sends nothing. Inputs: the pool, the queue and the rename. Output: the field. */
function PoolNameField({ pool, apply, onRename }: { pool: PoolSummary; apply: ApplyChange; onRename: (pool: PoolSummary, name: string) => MadeChange }) {
  const [shown, hold] = useHeld<string>(pool.name);
  const [typed, setTyped] = useState(shown);
  const [error, setError] = useState<string>();
  useEffect(() => {
    setTyped(shown);
    setError(undefined);
  }, [shown]);
  const commit = () => {
    const input = poolInput(typed, pool.members.map((member) => member.id));
    if (input.kind !== "ok") return setError(input.kind === "error" ? input.message : undefined);
    setError(undefined);
    if (input.value.name === shown) return setTyped(shown);
    applyHeld(apply, hold, input.value.name, onRename(pool, input.value.name));
  };
  return <TextField id="pool-name" label="Pool name" labelOutside aria-label="Pool name" value={typed} onValue={setTyped} error={error} onBlur={commit}
    onKeyDown={(event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      commit();
    }} />;
}

/** The accounts a pool can still take: the search and provider filters, without its members.
 * Inputs: the accounts, the filters and the members. Output: the accounts to list. */
export function addableAccounts(accounts: AccountView[], search: string, provider: string, members: readonly string[]): AccountView[] {
  return filterAccounts(accounts, search, provider, "").filter((account) => !members.includes(account.id));
}

/** The body of the add panel for each state of the read: loading, not ready, failed, no accounts
 * at all, nothing shown, or the windowed lines. Inputs: the read, the accounts shown and the line
 * for one account. Output: the list. */
function AddableList({ accounts, shown, line }: { accounts: Loaded<AccountView[]>; shown: AccountView[]; line: (account: AccountView) => ReactNode }) {
  if (accounts.kind === "loading") return <div className="set-body"><Skeleton shape="row" count={3} /></div>;
  if (accounts.kind === "not_ready") return <div className="set-body"><NotReady what="Accounts" compact /></div>;
  if (accounts.kind === "error") return <div className="set-body"><Notice tone="danger">Couldn't load accounts.</Notice></div>;
  if (accounts.value.length === 0) return <div className="set-body"><EmptyState compact icon="key-round" title="No accounts yet"
    action={<a className="btn" href="#/settings/accounts?action=add">Add account</a>}>Add an account first, then put it in a pool.</EmptyState></div>;
  if (shown.length === 0) return <p className="set-body secondary">No accounts match.</p>;
  return <WindowedList className="pool-add-list" label="Accounts you can add" count={shown.length} viewport={176} renderRow={(index) => line(shown[index]!)} />;
}

/** The panel that puts accounts in the pool: search, provider, Done and every account outside it.
 * Inputs: the pool, the accounts read, the add and the close actions. Output: the panel. */
function AddPanel({ pool, accounts, onAdd, onDone }: { pool: PoolSummary; accounts: Loaded<AccountView[]>; onAdd: (account: AccountView) => void; onDone: () => void }) {
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("");
  const all = accounts.kind === "ok" ? accounts.value : [];
  const members = pool.members.map((member) => member.id);
  const full = members.length >= POOL_MEMBER_MAX;
  const providers = [...new Map(all.map((account) => [account.provider, account.provider_name])).entries()];
  const line = (account: AccountView) => <div key={account.id} className="pool-add-row">
    <span className="pick-words"><span className="ellipsis">{account.name}</span><span className="small secondary ellipsis">{`${account.provider_name}. ${account.status_label}.`}</span></span>
    <Button size="small" aria-label={`Add ${account.name}`} data-control={`add:${account.id}`} aria-disabled={full || undefined} onClick={() => onAdd(account)}>Add</Button>
  </div>;
  return <div className="pool-add">
    <div className="set-bar">
      <SearchField label="Search accounts" value={search} onValue={setSearch} />
      <Select id="pool-provider" label="Provider" hideLabel value={provider} onValue={setProvider}
        options={[{ value: "", label: "All providers" }, ...providers.map(([value, label]) => ({ value, label }))]} />
      <Button variant="quiet" size="small" onClick={onDone}>Done</Button>
    </div>
    {full && <div className="set-body"><Notice>{POOL_FULL}</Notice></div>}
    <AddableList accounts={accounts} shown={addableAccounts(all, search, provider, members)} line={line} />
  </div>;
}

/** The selected pool on the right: its name, what it is used for, its actions, Add account and its
 * accounts in their real order. A pool that is not created yet has no order and no delete yet.
 * Inputs: the pool, whether it is a draft, the reads, now, the panel state, the pool being set up,
 * the saved line, the queue and the actions. Output: the pane. */
function PoolDetail({ pool, isDraft, accounts, schedule, ranking, limits, nowMs, adding, preparing, saved, apply, onToggleAdd, onDoneAdd, onAdd, onRemove, onRename, onDraftName, onDelete, onRetryOrder, onPrepare }: {
  pool: PoolSummary;
  isDraft: boolean;
  accounts: Loaded<AccountView[]>;
  schedule: Loaded<AccountSchedule>;
  ranking: Loaded<PoolRanking[]>;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
  adding: boolean;
  preparing: string | null;
  saved: string | undefined;
  apply: ApplyChange;
  onToggleAdd: () => void;
  onDoneAdd: () => void;
  onAdd: (pool: PoolSummary, account: AccountView) => void;
  onRemove: (pool: PoolSummary, id: string, name: string) => void;
  onRename: (pool: PoolSummary, name: string) => MadeChange;
  onDraftName: (name: string) => void;
  onDelete: (pool: PoolSummary) => void;
  onRetryOrder: () => void;
  onPrepare?: (pool: PoolSummary) => void;
}) {
  const note = notSignedInNote(pool.members.map((member) => member.id), new Map(pool.members.map((member) => [member.id, member.status])));
  const field = <div className="pool-name-edit">{isDraft
    ? <TextField id="pool-name" label="Pool name" labelOutside aria-label="Pool name" value={pool.name} onValue={onDraftName} />
    : <PoolNameField key={pool.id} pool={pool} apply={apply} onRename={onRename} />}</div>;
  const actions = isDraft ? undefined : <div className="pool-actions">
    {onPrepare && keyedMembers(pool).length > 0 && <Button icon="refresh-cw" aria-label={`Get ready for agents: ${pool.name}`} pending={preparing === pool.id} pendingLabel="Getting ready..." disabled={preparing !== null} onClick={() => onPrepare(pool)}>Get ready for agents</Button>}
    <Button variant="quiet" aria-label={`Delete pool ${pool.name}`} onClick={() => onDelete(pool)}>Delete pool</Button>
  </div>;
  return <div className="pool-detail">
    <SettingSection title={pool.name} titleField={field} about={`${poolUse(pool.project_count)}. ${poolReadiness(pool.members)}. Shown on each project's pool picker.`} actions={actions}>
      <div className="pool-cap">
        <CapacityMeter count={pool.members.length} />
        <Button size="small" icon="plus" aria-expanded={adding} data-control="add-account" onClick={onToggleAdd}>Add account</Button>
        <span className="pool-saved" role="status">{saved && <><Icon name="check" size={14} />{saved}</>}</span>
      </div>
      {adding && <KeepRowFocus gone={PANEL_GONE}><AddPanel pool={pool} accounts={accounts} onAdd={(account) => onAdd(pool, account)} onDone={onDoneAdd} /></KeepRowFocus>}
      {note && <div className="set-body"><Notice tone="warn">{note}</Notice></div>}
      {isDraft ? <p className="pool-empty">{DRAFT_EMPTY}</p>
        : <KeepRowFocus gone={ROWS_GONE}><PoolOrder key={pool.id} pool={pool} ranking={ranking} schedule={schedule} accounts={accounts} limits={limits} nowMs={nowMs} onRetry={onRetryOrder} apply={apply} onRemove={(id, name) => onRemove(pool, id, name)} /></KeepRowFocus>}
    </SettingSection>
  </div>;
}

/** Settings, Pools as a pure view: the pools and the app's order on the left, and on the right the
 * selected pool with its accounts in their real order. Inputs: the five reads, now, the selected
 * pool's id (the first pool when absent or gone), a started pool, whether the add panel is open,
 * the select, new, delete, retry, set-up, add, remove and rename actions, the pool being set up,
 * the saved line and the queue of changes. Output: the screen, or the state of the pools read. */
export function PoolsScreenView({ pools, accounts, schedule, ranking, limits, nowMs, selected, draft = null, adding = false, onSelect, onNew, onDelete, onRetry, onRetryOrder, onRetrySchedule, onPrepare, onToggleAdd, onDoneAdd, onAdd, onRemove, onRename, onDraftName, preparing = null, saved, apply }: {
  pools: Loaded<PoolSummary[]>;
  accounts: Loaded<AccountView[]>;
  schedule: Loaded<AccountSchedule>;
  ranking: Loaded<PoolRanking[]>;
  limits: Loaded<AccountLimits[]>;
  nowMs: number;
  selected: string | undefined;
  draft?: NewPool | null;
  adding?: boolean;
  onSelect: (id: string) => void;
  onNew: () => void;
  onDelete: (pool: PoolSummary) => void;
  onRetry: () => void;
  onRetryOrder: () => void;
  onRetrySchedule: () => void;
  onPrepare?: (pool: PoolSummary) => void;
  onToggleAdd: () => void;
  onDoneAdd: () => void;
  onAdd: (pool: PoolSummary, account: AccountView) => void;
  onRemove: (pool: PoolSummary, id: string, name: string) => void;
  onRename: (pool: PoolSummary, name: string) => MadeChange;
  onDraftName: (name: string) => void;
  preparing?: string | null;
  saved: string | undefined;
  apply: ApplyChange;
}) {
  if (pools.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (pools.kind === "not_ready") return <NotReady what="Pools" />;
  if (pools.kind === "error") return <LoadError message="Couldn't load pools." onRetry={onRetry} />;
  if (pools.value.length === 0 && draft === null) return <EmptyState icon="layers" title="No pools yet"
    action={<Button variant="primary" icon="plus" onClick={onNew}>New pool</Button>}>
    A pool groups up to 15 accounts for your projects to use.
  </EmptyState>;
  const started = draft === null ? null : draftSummary(draft);
  const pool = started ?? pools.value.find((item) => item.id === selected) ?? pools.value[0]!;
  return <div className="pool-screen">
    <div className="pool-left">
      <SettingSection title="Your pools" actions={<Button size="small" icon="plus" onClick={onNew}>New pool</Button>}>
        <div className="pool-items">
          {pools.value.map((item) => <PoolItem key={item.id} pool={item} on={item.id === pool.id} onSelect={() => onSelect(item.id)} />)}
          {started && <PoolItem pool={started} on onSelect={() => undefined} />}
        </div>
      </SettingSection>
      <AppOrder schedule={schedule} apply={apply} onRetry={onRetrySchedule} />
    </div>
    <PoolDetail pool={pool} isDraft={draft !== null && draft.id === null} accounts={accounts} schedule={schedule} ranking={ranking} limits={limits} nowMs={nowMs} adding={adding} preparing={preparing} saved={saved} apply={apply}
      onToggleAdd={onToggleAdd} onDoneAdd={onDoneAdd} onAdd={onAdd} onRemove={onRemove} onRename={onRename} onDraftName={onDraftName} onDelete={onDelete} onRetryOrder={onRetryOrder} onPrepare={onPrepare} />
  </div>;
}

/** A pool the owner started and the engine does not hold yet: on screen at once, and created by
 * its first account. `id` is set once it is created, until the next pools read has it. */
export interface NewPool {
  name: string;
  id: string | null;
  members: PoolMember[];
}

/** The first of "New pool", "New pool 2" and so on that no pool uses. Input: the names in use.
 * Output: the free name. */
export function freePoolName(taken: readonly string[]): string {
  for (let at = 1; at <= taken.length + 1; at += 1) {
    const name = at === 1 ? "New pool" : `New pool ${at}`;
    if (!taken.includes(name)) return name;
  }
  throw new Error("one of these names is always free");
}

/** Draws an account as a member of a pool not read yet. Input: the account. Output: the member. */
function memberOf(account: AccountView): PoolMember {
  return { id: account.id, kind: "other", provider: account.provider, status: account.status === "not_signed_in" ? "not_signed_in" : "signed_in", status_label: account.status_label };
}

/** The changes an owner makes to a pool's accounts and name, each through the queue, and the pool
 * being started. A queued Add on a draft reads whether the draft was created by then, so two quick
 * Adds create it once. Inputs: the queue, the accounts read, where a refusal is said and the
 * select. Output: the draft and its actions. */
function usePoolEdits(apply: ApplyChange, accounts: Loaded<AccountView[]>, refused: (sentence: string) => void, select: (id: string) => void) {
  const session = useAppSession();
  const known = accounts.kind === "ok" ? accounts.value : [];
  const [draft, setDraft] = useState<NewPool | null>(null);
  const run = useRef<{ id: string | null; name: string; members: string[] } | null>(null);

  const sent = async (write: () => Promise<Result<void>>, done: string): Promise<Result<string | null>> => {
    const result = session.noteResult(await write());
    if (result.kind === "not_ready") return { kind: "error", code: "not_ready", message: "Saving pools isn't ready yet in this build." };
    if (result.kind === "ok") return { kind: "ok", value: done };
    return result.code === "pool_member_limit" ? { ...result, message: POOL_FULL } : result;
  };

  const readyAfter = (answer: Promise<Result<string | null>>, ids: string[]) => {
    if (setupTargets([], ids, known).length === 0) return;
    void answer.then((result) => {
      if (result.kind !== "ok") return;
      void apply({ send: async () => {
        const note = await readyAdded([], ids, known, session.noteResult);
        return note === undefined ? { kind: "ok", value: null } : { kind: "error", code: "account_setup", message: note };
      } });
    });
  };

  const addedWords = (account: AccountView) => `Added ${account.name}${setupTargets([], [account.id], known).length > 0 ? ". Getting it ready for agents." : "."}`;

  const add = (pool: PoolSummary, account: AccountView) => {
    const before = pool.members.map((member) => member.id);
    const next = toggleMember(before, account.id);
    if (next.kind === "full") return refused(POOL_FULL);
    readyAfter(apply({ send: () => sent(() => savePool(pool.id, pool.name, next.members, { name: pool.name, members: before }), addedWords(account)) }), [account.id]);
  };

  const addToDraft = (account: AccountView) => {
    const target = run.current;
    if (target === null || draft === null) throw new Error("only an open draft takes an account");
    const name = draft.name;
    readyAfter(apply({ send: async () => {
      if (target.members.includes(account.id)) return { kind: "ok", value: null };
      const first = target.id === null;
      const id = target.id ?? newPoolId();
      const before = target.members;
      const made = first ? await sent(() => createPool(id, name, [account.id]), `${name.trim()} created.`)
        : await sent(() => savePool(id, target.name, [...before, account.id], { name: target.name, members: before }), addedWords(account));
      if (made.kind !== "ok") return made;
      target.id = id;
      if (first) target.name = name.trim();
      target.members = [...before, account.id];
      if (run.current !== target) return made;
      setDraft((now) => now && { ...now, id, members: [...now.members, memberOf(account)] });
      if (first) select(id);
      return made;
    } }), [account.id]);
  };

  const createWith = (ids: string[], taken: readonly string[]) => {
    const id = newPoolId();
    const name = freePoolName(taken);
    readyAfter(apply({ send: async () => {
      const made = await sent(() => createPool(id, name, ids), `${name} created.`);
      if (made.kind === "ok") select(id);
      return made;
    } }), ids);
  };

  const remove = (pool: PoolSummary, id: string, name: string) => {
    const before = pool.members.map((member) => member.id);
    void apply({ send: () => sent(() => savePool(pool.id, pool.name, before.filter((item) => item !== id), { name: pool.name, members: before }), `Removed ${name}. It stays in your list.`) });
  };

  const rename = (pool: PoolSummary, name: string): MadeChange => {
    const members = pool.members.map((member) => member.id);
    return { send: () => sent(() => savePool(pool.id, name, members, { name: pool.name, members }), "Pool name saved.") };
  };

  const open = (taken: readonly string[]) => {
    if (run.current?.id === null) return;
    run.current = { id: null, name: "", members: [] };
    setDraft({ name: freePoolName(taken), id: null, members: [] });
  };

  const drop = () => {
    run.current = null;
    setDraft(null);
  };

  return { draft, open, drop, setName: (name: string) => setDraft((now) => now && { ...now, name }), add, addToDraft, createWith, remove, rename };
}

/** Settings, Pools: the one screen, adding, removing, renaming, a new pool and Delete pool; the
 * pools, the accounts and the limits are read again once a minute, and the order too unless a
 * change waits, since the queue reads it when it drains. Input: whether the page was opened to
 * start a new pool. */
export function Pools({ creating }: { creating: boolean }) {
  const session = useAppSession();
  const [pools, reload] = useLoad(poolSummaries, [], "pool-summaries");
  const [accounts, rereadAccounts] = useLoad(listAccounts, [], "accounts");
  const [schedule, rereadSchedule] = useLoad(accountSchedule, [], "account-schedule");
  const [ranking, rerank] = useLoad(accountRanking, [], "account-ranking");
  const [usage, rereadUsage] = useLoad(() => loadUsage(Date.now() - LIMITS_RANGE_MS, offsetMinutes(new Date()), null), [], "pool-usage");
  const [selected, setSelected] = useState<string>();
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<PoolSummary>();
  const [users, setUsers] = useState<string[]>();
  const [removing, setRemoving] = useState(false);
  const [preparing, setPreparing] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<string>();
  const { saved, apply, changing } = usePoolChanges((poolsMoved) => {
    rereadSchedule();
    rerank();
    if (poolsMoved) { reload(); rereadAccounts(); }
  }, setRefusal);
  const edits = usePoolEdits(apply, accounts, setRefusal, setSelected);
  const read = pools.kind === "ok" ? pools.value : [];
  const draft = edits.draft?.id && read.some((item) => item.id === edits.draft?.id) ? null : edits.draft;
  useEffect(() => {
    if (edits.draft !== null && draft === null) edits.drop();
  }, [draft, edits.draft]);
  useEffect(() => {
    if (!creating || pools.kind !== "ok" || accounts.kind === "loading") return;
    const taken = takePoolDraft();
    navigate({ name: "settings", tab: "pools" }, true);
    const names = pools.value.map((item) => item.name);
    if (taken.length > 0) return edits.createWith(taken, names);
    edits.open(names);
    setAdding(true);
  }, [creating, pools.kind, accounts.kind]);
  useEffect(() => {
    const timer = setInterval(() => {
      setNowMs(Date.now());
      reload();
      rereadAccounts();
      if (!changing()) rerank();
      rereadUsage();
    }, REREAD_MS);
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

  const remove = async () => {
    if (!deleting) return;
    setRemoving(true);
    const result = session.noteResult(await deletePool(deleting.id));
    setRemoving(false);
    const pool = deleting;
    setDeleting(undefined);
    if (result.kind !== "ok") return setRefusal(result.kind === "not_ready" ? "Deleting pools isn't ready yet in this build." : result.message);
    edits.drop();
    const at = read.findIndex((item) => item.id === pool.id);
    setSelected((read[at + 1] ?? read[at - 1])?.id);
    setAdding(false);
    session.showToast(deleteCopy(pool, users).done);
    reread();
  };

  const askDelete = async (pool: PoolSummary) => {
    setRefusal(undefined);
    setUsers(undefined);
    setDeleting(pool);
    if (pool.project_count === 0) return;
    const names = await projectsUsing(pool.id);
    if (names.kind === "ok") setUsers(names.value);
  };

  const closeAdd = () => {
    document.querySelector<HTMLElement>(ROWS_GONE)?.focus();
    setAdding(false);
  };

  const copy = deleting && deleteCopy(deleting, users);

  return <div className="stack">
    <div className="settings-top">
      <div><h2>Pools</h2><p>A pool is a group of accounts for a project. When one account runs out, agents move to the next one in the pool.</p></div>
    </div>
    <div>
      {refusal && <div className="pool-refusal"><Notice tone="danger" alert><span className="grow">{refusal}</span></Notice></div>}
      <PoolsScreenView pools={pools} accounts={accounts} schedule={schedule} ranking={ranking} limits={limits} nowMs={nowMs} selected={selected} draft={draft} adding={adding} onRetryOrder={rerank} onRetrySchedule={rereadSchedule} saved={saved} apply={apply}
        onSelect={(id) => { edits.drop(); setAdding(false); setRefusal(undefined); setSelected(id); }} onNew={() => { edits.open(read.map((item) => item.name)); setRefusal(undefined); setAdding(true); }}
        onToggleAdd={() => setAdding(!adding)} onDoneAdd={closeAdd} onAdd={(pool, account) => (draft ? edits.addToDraft(account) : edits.add(pool, account))} onRemove={edits.remove} onRename={edits.rename} onDraftName={edits.setName}
        onDelete={(pool) => void askDelete(pool)} onRetry={reload} onPrepare={(pool) => void prepareAll(pool)} preparing={preparing} />
    </div>
    <ConfirmDialog open={Boolean(deleting)} title={`Delete ${deleting?.name ?? "pool"}?`} confirmLabel={copy?.confirmLabel ?? "Delete pool"} pending={removing}
      onCancel={() => setDeleting(undefined)} onConfirm={() => void remove()}>
      {copy?.body}
    </ConfirmDialog>
  </div>;
}
