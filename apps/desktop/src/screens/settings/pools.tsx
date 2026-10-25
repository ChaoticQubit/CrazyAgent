import { useEffect, useState } from "react";
import { deletePool, listAccounts, poolSummaries, readyForAgents, savePool, POOL_MEMBER_MAX, type AccountView, type PoolBase, type PoolMember, type PoolSummary, type Result } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import {
  Button, CapacityMeter, ConfirmDialog, EmptyState, LoadError, Menu, Notice, NotReady, Panel, ProviderMark, SearchField, Select, Skeleton,
  StatusBadge, TextField, WindowedList, navigate, Checkbox,
} from "../../ui";
import { filterAccounts } from "./accounts";
import { takePoolDraft } from "./pool-draft";

export const POOL_FULL = "A pool can hold up to 15 accounts. Remove one to add another.";

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

/** The message when the engine refuses to delete a pool a project uses. Input: the pool.
 * Output: the sentence. */
export function deleteRefusal(pool: PoolSummary): string {
  const count = pool.project_count;
  const users = count === 1 ? "1 project uses it" : count > 1 ? `${count} projects use it` : "A project uses it";
  return `Can't delete ${pool.name}. ${users}. Choose another pool for ${count === 1 ? "it" : "them"} first.`;
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

/** The pool list as a pure view. Inputs: the pools read and the new, edit, delete and retry
 * actions. */
export function PoolsView({ pools, onNew, onEdit, onDelete, onRetry, onPrepare }: {
  pools: Loaded<PoolSummary[]>;
  onNew: () => void;
  onEdit: (pool: PoolSummary) => void;
  onDelete: (pool: PoolSummary) => void;
  onRetry: () => void;
  onPrepare?: (pool: PoolSummary) => void;
}) {
  if (pools.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (pools.kind === "not_ready") return <NotReady what="Pools" />;
  if (pools.kind === "error") return <LoadError message="Couldn't load pools." onRetry={onRetry} />;
  if (pools.value.length === 0) return <EmptyState icon="layers" title="No pools yet"
    action={<Button variant="primary" icon="plus" onClick={onNew}>New pool</Button>}>
    A pool groups up to 15 accounts for your projects to use.
  </EmptyState>;
  return <div className="stack-small">
    <div className="toolbar"><span className="count">{pools.value.length} {pools.value.length === 1 ? "pool" : "pools"}</span>
      <Button variant="primary" icon="plus" onClick={onNew}>New pool</Button></div>
    <Panel padded={false}><div className="rows">
      {pools.value.map((pool) => <div className="pool-row" key={pool.id}>
        <button type="button" className="name ellipsis" onClick={() => onEdit(pool)}>{pool.name}</button>
        <span className="secondary ellipsis">{pool.summary}</span>
        <span className="mono">{pool.members.length} of {POOL_MEMBER_MAX}</span>
        <span className="secondary">{poolReadiness(pool.members)}</span>
        <span className="secondary">{poolUse(pool.project_count)}</span>
        <Menu label={`More for ${pool.name}`} items={[
          { label: "Edit pool", icon: "pencil", onSelect: () => onEdit(pool) },
          ...(onPrepare && keyedMembers(pool).length > 0 ? [{ label: "Get ready for agents", icon: "refresh-cw" as const, onSelect: () => onPrepare(pool) }] : []),
          { label: "Delete pool", icon: "trash", danger: true, onSelect: () => onDelete(pool) },
        ]} />
      </div>)}
    </div></Panel>
  </div>;
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
  const [members, setMembers] = useState<string[]>(pool?.members.map((member) => member.id) ?? initialMembers.slice(0, POOL_MEMBER_MAX));
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

  return <form className="stack" onSubmit={(event) => { event.preventDefault(); void save(); }}>
    <span><Button variant="quiet" icon="arrow-left" onClick={onCancel}>Back to pools</Button></span>
    <h2>{pool ? `Edit ${pool.name}` : "New pool"}</h2>
    <TextField id="pool-name" label="Pool name" value={name} onValue={setName} maxLength={60} error={nameError} />
    <div className="pool-editor">
      <Panel title="All accounts" padded={false}>
        <div className="toolbar panel-body">
          <SearchField label="Search accounts" value={search} onValue={setSearch} />
          <Select id="pool-provider" label="Provider" hideLabel value={provider} onValue={setProvider}
            options={[{ value: "", label: "All providers" }, ...providers.map(([value, label]) => ({ value, label }))]} />
          <Checkbox label="Selected only" checked={selectedOnly} onChecked={setSelectedOnly} />
        </div>
        {accounts.kind === "loading" ? <Skeleton shape="row" count={6} /> : accounts.kind === "not_ready" ? <NotReady what="Accounts" compact />
          : accounts.kind === "error" ? <Notice tone="danger">Couldn't load accounts.</Notice>
          : all.length === 0 ? <EmptyState compact icon="key-round" title="No accounts yet"
            action={<a className="btn" href="#/settings/accounts?action=add">Add account</a>}>Add an account first, then put it in a pool.</EmptyState>
          : shown.length === 0 ? <p className="item-row secondary">{selectedOnly && members.length === 0 ? "No accounts selected yet." : "No accounts match."}</p>
          : <WindowedList label="All accounts" count={shown.length} viewport={440} renderRow={(index) => {
            const account = shown[index]!;
            const checked = members.includes(account.id);
            const unavailable = full && !checked;
            return <label className={`pick${blocked === account.id ? " blocked" : ""}`} title={unavailable ? POOL_FULL : undefined}>
              <input type="checkbox" checked={checked} aria-disabled={unavailable || undefined} onChange={() => toggle(account.id)} />
              <ProviderMark provider={account.provider} name={account.provider_name} />
              <span className="stack-small"><span className="ellipsis">{account.name}</span>
                {blocked === account.id && <span className="small">{POOL_FULL}</span>}</span>
              <StatusBadge word={account.status_label} />
            </label>;
          }} />}
      </Panel>
      <Panel title="In this pool">
        <div className="stack-small">
          <CapacityMeter count={members.length} />
          {full && <Notice id="pool-full-notice" alert={blocked !== undefined}>{POOL_FULL}</Notice>}
          {note && <Notice tone="info">{note}</Notice>}
          {members.length === 0 ? <p className="secondary">No accounts yet. Check accounts on the left to add them.</p>
            : <ul className="plain-list">{members.map((id) => {
              const account = byId.get(id);
              const member = known.get(id);
              const label = account?.name ?? member?.provider ?? "Account";
              return <li className="member" key={id}>
                <ProviderMark provider={account?.provider ?? member?.provider ?? ""} name={account?.provider_name ?? member?.provider ?? ""} />
                <span className="grow ellipsis">{label}</span>
                <StatusBadge word={account?.status_label ?? member?.status_label ?? ""} />
                <Button iconOnly={`Remove ${label}`} icon="x" size="small" onClick={() => toggle(id)} />
              </li>;
            })}</ul>}
        </div>
      </Panel>
    </div>
    {saveError && <Notice tone="danger" alert>{saveError}</Notice>}
    <div className="row">
      <Button type="submit" variant="primary" icon="check" pending={saving} pendingLabel="Saving...">Save pool</Button>
      <Button onClick={onCancel}>Cancel</Button>
    </div>
  </form>;
}

/** Settings, Pools: the pool list, the pool editor and Delete pool.
 * Input: whether a new pool editor opens on arrival. */
export function Pools({ creating }: { creating: boolean }) {
  const session = useAppSession();
  const [pools, reload] = useLoad(poolSummaries);
  const [accounts] = useLoad(listAccounts);
  const [editing, setEditing] = useState<{ id?: string; opened?: PoolBase } | undefined>(creating ? {} : undefined);
  const [draft, setDraft] = useState<string[]>([]);
  const [deleting, setDeleting] = useState<PoolSummary>();
  const [removing, setRemoving] = useState(false);
  const [refusal, setRefusal] = useState<string>();
  useEffect(() => {
    if (!creating) return;
    setEditing({});
    const taken = takePoolDraft();
    if (taken.length) setDraft(taken);
  }, [creating]);

  const prepare = async (targets: { id: string; name: string }[]) => {
    const failures: { name: string; message: string }[] = [];
    for (const target of targets) {
      const ready = session.noteResult(await readyForAgents(target.id));
      if (ready.kind !== "ok") failures.push({ name: target.name, message: ready.kind === "not_ready" ? "Setting accounts up for agents isn't ready in this build." : ready.message });
    }
    return failures;
  };

  const prepareAll = async (pool: PoolSummary) => {
    setRefusal(undefined);
    const names = new Map(accounts.kind === "ok" ? accounts.value.map((account) => [account.id, account.name]) : []);
    const ids = keyedMembers(pool);
    const failures = await prepare(ids.map((id) => ({ id, name: names.get(id) ?? "An account" })));
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
      const targets = setupTargets(pool?.members.map((member) => member.id) ?? [], members, accounts.kind === "ok" ? accounts.value : []);
      setRefusal(setupNote(await prepare(targets.map((account) => ({ id: account.id, name: account.name })))));
      session.showToast(`${name.trim()} saved.`);
      reload();
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
    if (result.kind === "ok") { session.showToast(`${pool.name} deleted.`); reload(); }
    else setRefusal(result.kind === "error" && result.code === "pool_in_use" ? deleteRefusal(pool)
      : result.kind === "not_ready" ? "Deleting pools isn't ready yet in this build." : result.message);
  };

  return <div className="stack">
    {refusal && <Notice tone="danger" alert><span className="grow">{refusal}</span></Notice>}
    <PoolsView pools={pools} onNew={() => setEditing({})} onEdit={(pool) => setEditing({ id: pool.id, opened: { name: pool.name, members: pool.members.map((member) => member.id) } })}
      onDelete={(pool) => { setRefusal(undefined); setDeleting(pool); }} onRetry={reload} onPrepare={(pool) => void prepareAll(pool)} />
    <ConfirmDialog open={Boolean(deleting)} title={`Delete ${deleting?.name ?? "pool"}?`} confirmLabel="Delete pool" pending={removing}
      onCancel={() => setDeleting(undefined)} onConfirm={() => void remove()}>
      {deleting && deleting.project_count > 0 ? `${poolUse(deleting.project_count)}. The engine refuses while a project uses it.`
        : "Its accounts stay in your list. Only the pool goes away."}
    </ConfirmDialog>
  </div>;
}

