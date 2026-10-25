import { useEffect, useState } from "react";
import { listAccounts, poolSummaries, removeAccount, POOL_MEMBER_MAX, type AccountView, type PoolSummary, type Result } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import {
  Button, ConfirmDialog, EmptyState, LoadError, Menu, Notice, NotReady, Panel, ProviderMark, SearchField, Select, Skeleton, StatusBadge, WindowedList,
  navigate, type IconName,
} from "../../ui";
import { AccountDrawer, andList, removeBody } from "./account-drawer";
import { AddAccountDialog } from "./add-account";
import { startPoolWith } from "./pool-draft";
import { cancelSignIn } from "./sign-in";

/** One thing the owner can do to an account from its row. */
export type AccountAction = "sign_in" | "cancel_sign_in" | "details" | "delete";

const ACTION_ITEMS: Record<AccountAction, { label: string; icon: IconName }> = {
  sign_in: { label: "Sign in again", icon: "log-in" },
  cancel_sign_in: { label: "Cancel sign-in", icon: "x" },
  details: { label: "Details and rename", icon: "pencil" },
  delete: { label: "Delete", icon: "trash" },
};

const POLL_MS = 3000;

/** The actions an account offers: Sign in again for a subscription that is not signed in,
 * Cancel sign-in while the engine reports one running, then details and Delete.
 * Input: the account. Output: the actions in menu order. */
export function accountActions(account: AccountView): AccountAction[] {
  const subscription = account.method === "subscription";
  const running = subscription && account.status === "signing_in";
  const signIn = subscription && !running && account.status !== "signed_in";
  return [...running ? ["cancel_sign_in" as const] : [], ...signIn ? ["sign_in" as const] : [], "details", "delete"];
}

/** Whether the list should be read again soon: an account shows a sign-in in progress, so the
 * page follows the engine until it ends. Input: the accounts. Output: true while one is. */
export function signInPolling(accounts: readonly AccountView[]): boolean {
  return accounts.some((account) => account.status === "signing_in");
}

/** Splits accounts to delete into those the engine can remove and those a pool still uses,
 * which it refuses. Inputs: the accounts and the pools read (undefined when unknown).
 * Output: the accounts to remove and a plain line for each one left out. */
export function deletePlan(accounts: readonly AccountView[], pools: readonly PoolSummary[] | undefined): {
  remove: AccountView[];
  blocked: { name: string; message: string }[];
} {
  const remove: AccountView[] = [];
  const blocked: { name: string; message: string }[] = [];
  for (const account of accounts) {
    const names = (pools ?? []).filter((pool) => pool.members.some((member) => member.id === account.id)).map((pool) => pool.name);
    if (names.length === 0) remove.push(account);
    else blocked.push({ name: account.name, message: `${account.name} ${removeBody(names).replace(/^It /, "")}` });
  }
  return { remove, blocked };
}

/** Deletes accounts one at a time, cancelling a running sign-in first so none is left behind.
 * Inputs: the accounts and the session's result recorder. Output: the names removed and the
 * failures with plain messages. */
export async function deleteAccounts(accounts: readonly AccountView[], note: <T>(result: Result<T>) => Result<T>): Promise<{
  removed: string[];
  failures: { name: string; message: string }[];
}> {
  const removed: string[] = [];
  const failures: { name: string; message: string }[] = [];
  for (const account of accounts) {
    if (account.status === "signing_in") note(await cancelSignIn(account.id));
    const result = note(await removeAccount(account.id));
    if (result.kind === "ok") removed.push(account.name);
    else failures.push({ name: account.name, message: result.kind === "not_ready" ? "Deleting accounts isn't ready in this build." : result.message });
  }
  return { removed, failures };
}

/** The line after a delete. Inputs: the names removed and the ones that were not, each with
 * a plain message. Output: "Deleted A and B.", with the first failure's reason when any. */
export function deleteSummary(removed: readonly string[], failures: readonly { name: string; message: string }[]): string {
  const done = removed.length ? `Deleted ${andList(removed)}.` : "";
  const left = failures.length ? `Couldn't delete ${andList(failures.map((failure) => failure.name))}. ${failures[0]!.message}` : "";
  return [done, left].filter(Boolean).join(" ");
}

/** Filters accounts by search text (name, provider or method), provider and status.
 * Inputs: accounts and the three filters ("" means all). Output: the matching accounts. */
export function filterAccounts(accounts: AccountView[], search: string, provider: string, status: string): AccountView[] {
  const needle = search.trim().toLowerCase();
  return accounts.filter((account) => (!provider || account.provider === provider) && (!status || account.status === status) &&
    (!needle || [account.name, account.provider_name, account.method_label].some((text) => text.toLowerCase().includes(needle))));
}

/** The account count line: "214 accounts", or "12 of 214 accounts" when filtered.
 * Inputs: shown and total counts. Output: the line. */
export function accountCount(shown: number, total: number): string {
  const noun = total === 1 ? "account" : "accounts";
  return shown === total ? `${total.toLocaleString("en-US")} ${noun}` : `${shown.toLocaleString("en-US")} of ${total.toLocaleString("en-US")} ${noun}`;
}

const STATUS_OPTIONS = [
  { value: "", label: "All statuses" }, { value: "signed_in", label: "Signed in" }, { value: "not_signed_in", label: "Not signed in" },
  { value: "key_saved", label: "Key saved" }, { value: "unreachable", label: "Can't reach this URL" }, { value: "signing_in", label: "Signing in..." },
];

/** Toggles one account in the Accounts page selection. Inputs: the selected ids and the
 * account id. Output: the new selection, in the order picked. */
export function toggleSelected(selected: readonly string[], id: string): string[] {
  return selected.includes(id) ? selected.filter((item) => item !== id) : [...selected, id];
}

/** What "New pool from selected" can do with a selection: start a pool, or why not.
 * Input: the selected ids that still exist. Output: ready, or the plain reason. */
export function poolFromSelection(selected: readonly string[]): { kind: "ready" } | { kind: "blocked"; reason: string } {
  if (selected.length === 0) return { kind: "blocked", reason: "Select accounts first." };
  if (selected.length > POOL_MEMBER_MAX) return { kind: "blocked", reason: `A pool holds up to ${POOL_MEMBER_MAX} accounts. ${selected.length} are selected.` };
  return { kind: "ready" };
}

/** The account list as a pure view: toolbar with search, filters, count and Add account,
 * then windowed rows with a checkbox and an actions menu each; selected accounts can start a
 * new pool, be deleted, or have their running sign-ins cancelled.
 * Inputs: the accounts and pools reads, the add, open, new pool, retry, row action and
 * selection handlers, and the accounts selected at first. */
export function AccountsView({ accounts, pools, onAdd, onOpen, onRetry, onNewPool, onAction, onDeleteSelected, onCancelSelected, initialSelection = [] }: {
  accounts: Loaded<AccountView[]>;
  pools: Loaded<PoolSummary[]>;
  onAdd: () => void;
  onOpen: (account: AccountView) => void;
  onRetry: () => void;
  onNewPool?: (ids: string[]) => void;
  onAction?: (account: AccountView, action: AccountAction) => void;
  onDeleteSelected?: (accounts: AccountView[]) => void;
  onCancelSelected?: (accounts: AccountView[]) => void;
  initialSelection?: string[];
}) {
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("");
  const [status, setStatus] = useState("");
  const [picked, setPicked] = useState<string[]>(initialSelection);
  if (accounts.kind === "loading") return <Skeleton shape="row" count={10} />;
  if (accounts.kind === "not_ready") return <NotReady what="Accounts" />;
  if (accounts.kind === "error") return <LoadError message="Couldn't load accounts." onRetry={onRetry} />;
  if (accounts.value.length === 0) return <EmptyState icon="key-round" title="No accounts yet"
    action={<Button variant="primary" icon="plus" onClick={onAdd}>Add account</Button>}>
    Add a ChatGPT, Claude, API key or local model account. You can add as many as you like.
  </EmptyState>;
  const providers = [...new Map(accounts.value.map((account) => [account.provider, account.provider_name])).entries()];
  const shown = filterAccounts(accounts.value, search, provider, status);
  const poolCount = (id: string) => pools.kind === "ok" ? pools.value.filter((pool) => pool.members.some((member) => member.id === id)).length : undefined;
  const exists = new Set(accounts.value.map((account) => account.id));
  const selected = picked.filter((id) => exists.has(id));
  const fromSelection = poolFromSelection(selected);
  const chosen = accounts.value.filter((account) => selected.includes(account.id));
  const running = chosen.filter((account) => account.method === "subscription" && account.status === "signing_in");
  const selectable = Boolean(onNewPool || onDeleteSelected);
  const classes = [selectable ? "accounts-selectable" : "", onAction ? "accounts-actions" : ""].filter(Boolean).join(" ");
  return <div className={classes || undefined}>
    <div className="toolbar">
      <SearchField label="Search accounts" value={search} onValue={setSearch} />
      <Select id="account-provider" label="Provider" hideLabel value={provider} onValue={setProvider}
        options={[{ value: "", label: "All providers" }, ...providers.map(([value, label]) => ({ value, label }))]} />
      <Select id="account-status" label="Status" hideLabel value={status} onValue={setStatus} options={STATUS_OPTIONS} />
      <span className="count" aria-live="polite">{accountCount(shown.length, accounts.value.length)}</span>
      <Button variant="primary" icon="plus" onClick={onAdd}>Add account</Button>
    </div>
    {selectable && selected.length > 0 && <div className="toolbar selection" role="group" aria-label="Selected accounts">
      <span className="count" aria-live="polite">{selected.length} selected</span>
      {onNewPool && <Button icon="layers" disabled={fromSelection.kind !== "ready"} onClick={() => onNewPool(selected)}>New pool from selected</Button>}
      {onCancelSelected && running.length > 0 && <Button icon="x" onClick={() => onCancelSelected(running)}>Cancel sign-in</Button>}
      {onDeleteSelected && <Button variant="danger-text" icon="trash" onClick={() => onDeleteSelected(chosen)}>Delete selected</Button>}
      <Button variant="quiet" onClick={() => setPicked([])}>Clear selection</Button>
      {fromSelection.kind === "blocked" && <span className="small secondary" role="status">{fromSelection.reason}</span>}
    </div>}
    <Panel padded={false}>
      <div className="list-head" aria-hidden="true"><span>Name</span><span>Provider</span><span>Sign-in method</span><span>Status</span><span>Pools</span></div>
      {shown.length === 0 ? <p className="item-row">No accounts match.
        <Button variant="quiet" onClick={() => { setSearch(""); setProvider(""); setStatus(""); }}>Clear filters</Button></p>
        : <WindowedList label="Accounts" count={shown.length} renderRow={(index) => {
          const account = shown[index];
          const row = <button type="button" className="account-row" onClick={() => onOpen(account)}>
            <span className="cell"><ProviderMark provider={account.provider} name={account.provider_name} /><span className="ellipsis">{account.name}</span></span>
            <span className="ellipsis secondary">{account.provider_name}</span>
            <span className="ellipsis secondary">{account.method_label}</span>
            <span><StatusBadge word={account.status_label} /></span>
            <span className="secondary">{poolCount(account.id) ?? ""}</span>
          </button>;
          const menu = onAction && <Menu label={`More for ${account.name}`} items={accountActions(account).map((action) => ({
            label: ACTION_ITEMS[action].label, icon: ACTION_ITEMS[action].icon, danger: action === "delete", onSelect: () => onAction(account, action),
          }))} />;
          if (!selectable && !menu) return row;
          return <div className="account-line">
            {selectable && <input type="checkbox" className="account-pick" aria-label={`Select ${account.name}`} checked={selected.includes(account.id)}
              onChange={() => setPicked((current) => toggleSelected(current.filter((id) => exists.has(id)), account.id))} />}
            {row}
            {menu}
          </div>;
        }} />}
    </Panel>
  </div>;
}

/** Settings, Accounts: the account list with row and selection actions, the Delete
 * confirmation, the Add account dialog and the account drawer. While an account shows a
 * sign-in in progress the list is read again every few seconds.
 * Input: whether the Add account dialog opens on arrival. */
export function Accounts({ adding }: { adding: boolean }) {
  const session = useAppSession();
  const [accounts, reload] = useLoad(listAccounts);
  const [pools, reloadPools] = useLoad(poolSummaries);
  const [opened, setOpened] = useState<{ account: AccountView; signIn: boolean }>();
  const [add, setAdd] = useState(adding);
  const [deleting, setDeleting] = useState<AccountView[]>();
  const [working, setWorking] = useState(false);
  const [problem, setProblem] = useState<string>();
  useEffect(() => { if (adding) setAdd(true); }, [adding]);
  const list = accounts.kind === "ok" ? accounts.value : [];
  const known = pools.kind === "ok" ? pools.value : undefined;
  const current = opened && (list.find((account) => account.id === opened.account.id) ?? opened.account);
  const refresh = () => { reload(); reloadPools(); };
  const polling = signInPolling(list);
  useEffect(() => {
    if (!polling) return;
    const timer = setTimeout(reload, POLL_MS);
    return () => clearTimeout(timer);
  }, [accounts, polling, reload]);

  const cancel = async (chosen: AccountView[]) => {
    setProblem(undefined);
    const failures: { name: string; message: string }[] = [];
    for (const account of chosen) {
      const result = session.noteResult(await cancelSignIn(account.id));
      if (result.kind !== "ok") failures.push({ name: account.name, message: result.kind === "not_ready" ? "Cancelling sign-in isn't ready in this build." : result.message });
    }
    if (failures.length) setProblem(`Couldn't cancel the sign-in for ${andList(failures.map((failure) => failure.name))}. ${failures[0]!.message}`);
    else session.showToast(chosen.length === 1 ? "Sign-in cancelled." : "Sign-ins cancelled.");
    refresh();
  };

  const remove = async () => {
    if (!deleting) return;
    setWorking(true);
    const plan = deletePlan(deleting, known);
    const result = await deleteAccounts(plan.remove, session.noteResult);
    setWorking(false);
    setDeleting(undefined);
    const failures = [...plan.blocked, ...result.failures];
    if (opened && plan.remove.some((account) => account.id === opened.account.id)) setOpened(undefined);
    if (failures.length) setProblem(deleteSummary(result.removed, failures));
    else { setProblem(undefined); session.showToast(deleteSummary(result.removed, [])); }
    refresh();
  };

  const act = (account: AccountView, action: AccountAction) => {
    if (action === "details" || action === "sign_in") setOpened({ account, signIn: action === "sign_in" });
    else if (action === "cancel_sign_in") void cancel([account]);
    else setDeleting([account]);
  };

  const plan = deleting && deletePlan(deleting, known);
  return <>
    {problem && <Notice tone="danger" alert><span className="grow">{problem}</span></Notice>}
    <AccountsView accounts={accounts} pools={pools} onAdd={() => setAdd(true)} onOpen={(account) => setOpened({ account, signIn: false })} onRetry={reload}
      onNewPool={(ids) => { startPoolWith(ids); navigate({ name: "settings", tab: "pools", action: "new" }); }}
      onAction={act} onDeleteSelected={setDeleting} onCancelSelected={(chosen) => void cancel(chosen)} />
    <AddAccountDialog open={add} accounts={list} onAdded={refresh} onClose={() => {
      setAdd(false);
      if (adding) navigate({ name: "settings", tab: "accounts" }, true);
    }} />
    <AccountDrawer account={current} pools={known} signIn={opened?.signIn} onClose={() => setOpened(undefined)}
      onChanged={(account) => { if (account) setOpened({ account, signIn: false }); refresh(); }} onDelete={(account) => setDeleting([account])} />
    <ConfirmDialog open={Boolean(deleting)} pending={working} onCancel={() => setDeleting(undefined)} onConfirm={() => void remove()}
      title={deleting?.length === 1 ? `Delete ${deleting[0]!.name}?` : `Delete ${deleting?.length ?? 0} accounts?`}
      confirmLabel={deleting?.length === 1 ? "Delete account" : `Delete ${deleting?.length ?? 0} accounts`}>
      {plan && <span className="stack-small">
        {plan.remove.length > 0 && <span>{andList(plan.remove.map((account) => account.name))} {plan.remove.length === 1 ? "and its saved sign-in or key are" : "and their saved sign-ins or keys are"} removed from CrazyAgent. A running sign-in is cancelled first.</span>}
        {plan.blocked.map((item) => <span key={item.name}>{item.message}</span>)}
      </span>}
    </ConfirmDialog>
  </>;
}
