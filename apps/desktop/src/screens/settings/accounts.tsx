import { useEffect, useState } from "react";
import { listAccounts, poolSummaries, type AccountView, type PoolSummary } from "../../api";
import { useLoad, type Loaded } from "../../state";
import { Button, EmptyState, LoadError, NotReady, Panel, ProviderMark, SearchField, Select, Skeleton, StatusBadge, WindowedList, navigate } from "../../ui";
import { AccountDrawer } from "./account-drawer";
import { AddAccountDialog } from "./add-account";

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

/** The account list as a pure view: toolbar with search, filters, count and Add account,
 * then windowed rows. Inputs: the accounts and pools reads, and the add and open handlers. */
export function AccountsView({ accounts, pools, onAdd, onOpen, onRetry }: {
  accounts: Loaded<AccountView[]>;
  pools: Loaded<PoolSummary[]>;
  onAdd: () => void;
  onOpen: (account: AccountView) => void;
  onRetry: () => void;
}) {
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("");
  const [status, setStatus] = useState("");
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
  return <div>
    <div className="toolbar">
      <SearchField label="Search accounts" value={search} onValue={setSearch} />
      <Select id="account-provider" label="Provider" hideLabel value={provider} onValue={setProvider}
        options={[{ value: "", label: "All providers" }, ...providers.map(([value, label]) => ({ value, label }))]} />
      <Select id="account-status" label="Status" hideLabel value={status} onValue={setStatus} options={STATUS_OPTIONS} />
      <span className="count" aria-live="polite">{accountCount(shown.length, accounts.value.length)}</span>
      <Button variant="primary" icon="plus" onClick={onAdd}>Add account</Button>
    </div>
    <Panel padded={false}>
      <div className="list-head" aria-hidden="true"><span>Name</span><span>Provider</span><span>Sign-in method</span><span>Status</span><span>Pools</span></div>
      {shown.length === 0 ? <p className="item-row">No accounts match.
        <Button variant="quiet" onClick={() => { setSearch(""); setProvider(""); setStatus(""); }}>Clear filters</Button></p>
        : <WindowedList label="Accounts" count={shown.length} renderRow={(index) => {
          const account = shown[index];
          return <button type="button" className="account-row" onClick={() => onOpen(account)}>
            <span className="cell"><ProviderMark provider={account.provider} name={account.provider_name} /><span className="ellipsis">{account.name}</span></span>
            <span className="ellipsis secondary">{account.provider_name}</span>
            <span className="ellipsis secondary">{account.method_label}</span>
            <span><StatusBadge word={account.status_label} /></span>
            <span className="secondary">{poolCount(account.id) ?? ""}</span>
          </button>;
        }} />}
    </Panel>
  </div>;
}

/** Settings, Accounts: the account list, the Add account dialog and the account drawer.
 * Input: whether the Add account dialog opens on arrival. */
export function Accounts({ adding }: { adding: boolean }) {
  const [accounts, reload] = useLoad(listAccounts);
  const [pools, reloadPools] = useLoad(poolSummaries);
  const [opened, setOpened] = useState<AccountView>();
  const [add, setAdd] = useState(adding);
  useEffect(() => { if (adding) setAdd(true); }, [adding]);
  const list = accounts.kind === "ok" ? accounts.value : [];
  const current = opened && (list.find((account) => account.id === opened.id) ?? opened);
  const refresh = () => { reload(); reloadPools(); };
  return <>
    <AccountsView accounts={accounts} pools={pools} onAdd={() => setAdd(true)} onOpen={setOpened} onRetry={reload} />
    <AddAccountDialog open={add} accounts={list} onAdded={refresh} onClose={() => {
      setAdd(false);
      if (adding) navigate({ name: "settings", tab: "accounts" }, true);
    }} />
    <AccountDrawer account={current} pools={pools.kind === "ok" ? pools.value : undefined} onClose={() => setOpened(undefined)}
      onChanged={(account) => { if (account) setOpened(account); refresh(); }} />
  </>;
}
