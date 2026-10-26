import { useEffect, useRef, useState } from "react";
import { accountUse, checkAccount, plainError, releaseAccount, renameAccount, type AccountUse, type AccountView, type PoolSummary } from "../../api";
import { useAppSession, type ProjectRow } from "../../state";
import { Button, Drawer, Notice, ProviderMark, StatusBadge, TextField } from "../../ui";
import { displayText } from "../conversation";
import { nameError } from "./add-account";
import { cancelSignIn, signOut, SignInPanel, signInStage, useSignIn, type SubscriptionProvider } from "./sign-in";

/** Joins names for a sentence: "A", "A and B", "A, B and C". Input: the names. Output: the text. */
export function andList(names: readonly string[]): string {
  return names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** What deleting accounts does to pools: the engine takes them out of every pool and deletes a
 * pool left with no account. Inputs: the account ids and the pools read (undefined when unknown).
 * Output: the sentence naming those pools, or undefined when no pool holds them. */
export function poolEffect(ids: readonly string[], pools: readonly PoolSummary[] | undefined): string | undefined {
  const left = (pools ?? []).filter((pool) => pool.members.some((member) => ids.includes(member.id)));
  if (left.length === 0) return undefined;
  const emptied = left.filter((pool) => pool.members.every((member) => ids.includes(member.id))).map((pool) => pool.name);
  const text = `${ids.length === 1 ? "It leaves" : "They leave"} ${left.length === 1 ? "pool" : "pools"} ${andList(left.map((pool) => pool.name))}.`;
  if (emptied.length === 0) return text;
  return emptied.length === 1 ? `${text} ${emptied[0]} has no other account, so it is deleted too and its projects have no pool.`
    : `${text} ${andList(emptied)} have no other account, so they are deleted too and their projects have no pool.`;
}

/** The support text for one account: names and statuses only, never a key, URL secret or
 * id. Input: the account. Output: plain lines. */
export function supportDetails(account: AccountView): string {
  return [`Account: ${account.name}`, `Provider: ${account.provider_name}`, `Sign-in method: ${account.method_label}`,
    `Status: ${account.status_label}`, ...account.model ? [`Model: ${account.model}`] : []].join("\n");
}

/** Whether an account can be signed out here: a subscription sign-in that is signed in.
 * Input: the account. Output: true when Sign out applies. */
export function canSignOut(account: AccountView): boolean {
  return account.method === "subscription" && (account.provider === "chatgpt" || account.provider === "claude")
    && account.status === "signed_in";
}

/** Names what keeps an account from signing out, grouped by project: "Team sub-agent 3 and
 * the master conversation in Alpha are using this account." Inputs: the account's use and
 * the known projects. Output: the sentence. */
export function inUseLine(used: AccountUse, projects: readonly ProjectRow[] | undefined): string {
  const named = [
    ...used.agents.map((agent) => ({ project: agent.project, name: displayText(agent.label) })),
    ...used.holders.map((holder) => ({ project: holder.project, name: holder.kind === "master" ? "the master conversation" : displayText(holder.title) })),
  ];
  const groups = new Map<string, string[]>();
  for (const { project, name } of named) groups.set(project, [...groups.get(project) ?? [], name]);
  const parts = [...groups].map(([project, names]) =>
    `${andList(names)} in ${displayText(projects?.find((row) => row.id === project)?.name ?? "another project")}`);
  const line = `${andList(parts)} ${named.length === 1 ? "is" : "are"} using this account.`;
  return line.charAt(0).toUpperCase() + line.slice(1);
}

/** Names the one action that ends what uses an account and signs it out: agents and
 * research jobs stop, master conversations end. Input: the account's use. Output: the
 * action's label and its label while it runs. */
export function releaseLabels(used: AccountUse): { label: string; pending: string } {
  const master = used.holders.some((holder) => holder.kind === "master");
  const others = used.agents.length > 0 || used.holders.some((holder) => holder.kind === "research");
  if (master && others) return { label: "Stop agents, end conversation and sign out", pending: "Ending..." };
  if (master) return { label: "End conversation and sign out", pending: "Ending conversation..." };
  return { label: "Stop agents and sign out", pending: "Stopping agents..." };
}

/** Says why an account could not be signed out. When the engine named what uses it, offers
 * one action that ends it all and signs out. Inputs: the account's use, the known
 * projects, whether that action runs, and its handler. */
export function InUseNotice({ used, projects, pending, onStop }: {
  used: AccountUse;
  projects: readonly ProjectRow[] | undefined;
  pending: boolean;
  onStop: () => void;
}) {
  if (used.agents.length === 0 && used.holders.length === 0) return <Notice tone="warn" alert>{plainError("native_profile_in_use")}</Notice>;
  const labels = releaseLabels(used);
  return <Notice tone="warn" alert><span className="stack-small"><span>{inUseLine(used, projects)}</span>
    <span><Button variant="danger-text" icon="log-out" pending={pending} pendingLabel={labels.pending} onClick={onStop}>{labels.label}</Button></span>
  </span></Notice>;
}

function SignInAgain({ account, onSignedIn }: { account: AccountView; onSignedIn: () => void }) {
  const provider = account.provider as SubscriptionProvider;
  const signIn = useSignIn(account.id);
  const stage = signInStage(signIn.state, provider);
  const signedIn = stage.kind === "signed_in";
  const reported = useRef(false);
  useEffect(() => {
    if (!signedIn || reported.current) return;
    reported.current = true;
    onSignedIn();
  }, [signedIn, onSignedIn]);
  return <SignInPanel provider={provider} method={provider === "claude" ? "claude_official" : "codex_device_code"} state={signIn.state}
    onStart={(method) => void signIn.start(method)} onCancel={() => void signIn.cancel()} onCheck={() => void signIn.check()}
    onClaudeCode={signIn.sendClaudeCode} />;
}

/** The account details drawer: rename, status with its action (Sign in again, Cancel sign-in,
 * Sign out, Check again), model, pools, support copy and Delete account. Inputs: the account (or
 * undefined when closed), pools, whether sign-in opens at once, and handlers for close, for any
 * change that needs the list reloaded and for Delete, which the page confirms. */
export function AccountDrawer({ account, pools, signIn = false, onClose, onChanged, onDelete }: {
  account: AccountView | undefined;
  pools: PoolSummary[] | undefined;
  signIn?: boolean;
  onClose: () => void;
  onChanged: (account?: AccountView) => void;
  onDelete: (account: AccountView) => void;
}) {
  const session = useAppSession();
  const [name, setName] = useState(account?.name ?? "");
  const [nameMessage, setNameMessage] = useState<string>();
  const [renaming, setRenaming] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const [checking, setChecking] = useState(false);
  const [checkLine, setCheckLine] = useState<string>();
  const [cancelling, setCancelling] = useState(false);
  const [signingOut, setSigningOut] = useState(false);
  const [inUse, setInUse] = useState<AccountUse>();
  const [error, setError] = useState<string>();
  const accountId = account?.id;
  useEffect(() => {
    setName(account?.name ?? "");
    setNameMessage(undefined);
    setSigningIn(signIn);
    setCheckLine(undefined);
    setInUse(undefined);
    setError(undefined);
  }, [accountId, signIn]);

  if (!account) return null;
  const memberOf = pools?.filter((pool) => pool.members.some((member) => member.id === account.id)) ?? [];
  const subscription = account.method === "subscription" && (account.provider === "chatgpt" || account.provider === "claude");

  const rename = async () => {
    const invalid = nameError(name);
    if (invalid) return setNameMessage(invalid);
    setRenaming(true);
    const result = session.noteResult(await renameAccount(account.id, name.trim()));
    setRenaming(false);
    if (result.kind === "ok") { setNameMessage(undefined); session.showToast("Account renamed."); onChanged(result.value); }
    else setNameMessage(result.kind === "not_ready" ? "Renaming isn't ready yet in this build." : result.message);
  };

  const check = async () => {
    setChecking(true);
    const result = session.noteResult(await checkAccount(account.id));
    setChecking(false);
    if (result.kind === "ok") {
      const count = result.value.models.length;
      setCheckLine(result.value.account.status === "unreachable" ? "Can't reach this URL. Check the address and that the server is running."
        : `Connected. ${count} ${count === 1 ? "model" : "models"} found.`);
      onChanged(result.value.account);
    } else setCheckLine(result.kind === "not_ready" ? "Checking isn't ready yet in this build." : result.message);
  };

  const cancel = async () => {
    setCancelling(true);
    const result = session.noteResult(await cancelSignIn(account.id));
    setCancelling(false);
    if (result.kind === "ok") { setError(undefined); session.showToast("Sign-in cancelled."); onChanged(); }
    else setError(result.kind === "not_ready" ? "Cancelling sign-in isn't ready yet in this build." : result.message);
  };

  const signOutAccount = async (stopFirst: boolean) => {
    setSigningOut(true);
    setError(undefined);
    if (stopFirst) {
      const released = session.noteResult(await releaseAccount(account.id));
      if (released.kind !== "ok") {
        setSigningOut(false);
        return setError(released.kind === "not_ready" ? "Stopping what uses this account isn't ready yet in this build." : released.message);
      }
      if (released.value.agents.length > 0 || released.value.holders.length > 0) {
        setSigningOut(false);
        return setInUse(released.value);
      }
    }
    const result = session.noteResult(await signOut(account.id));
    if (result.kind === "ok") {
      setSigningOut(false);
      setInUse(undefined);
      session.showToast("Signed out.");
      return onChanged();
    }
    if (result.kind === "error" && result.code === "native_profile_in_use") {
      const used = await accountUse(account.id);
      setSigningOut(false);
      return setInUse(used.kind === "ok" ? used.value : { agents: [], holders: [] });
    }
    setSigningOut(false);
    setError(result.kind === "not_ready" ? "Signing out isn't ready yet in this build." : result.message);
  };

  return <>
    <Drawer open title={account.name} onClose={onClose}
      footer={<span className="row"><Button variant="danger-text" icon="trash" onClick={() => onDelete(account)}>Delete account</Button>
        {memberOf.length > 0 && <span className="small secondary">{poolEffect([account.id], pools)}</span>}</span>}>
      <div className="row"><ProviderMark provider={account.provider} name={account.provider_name} large />
        <div className="stack-small"><strong>{account.provider_name}</strong><span className="secondary">{account.method_label}</span></div></div>
      <form className="row" onSubmit={(event) => { event.preventDefault(); void rename(); }}>
        <TextField id="drawer-account-name" label="Name" value={name} onValue={setName} maxLength={60} error={nameMessage} />
        <Button type="submit" pending={renaming} pendingLabel="Renaming..." disabled={name.trim() === account.name}>Rename</Button>
      </form>
      <dl className="details-grid">
        <dt>Status</dt><dd><StatusBadge word={account.status_label} /></dd>
        {account.url && <><dt>Base URL</dt><dd className="mono small ellipsis">{account.url}</dd></>}
        {account.model && <><dt>Default model</dt><dd className="mono small">{account.model}</dd></>}
        {account.method === "api_key" && <><dt>Key</dt><dd>Key saved</dd></>}
        <dt>Pools</dt><dd>{memberOf.length === 0 ? <span className="secondary">Not in a pool</span>
          : <ul className="plain-list">{memberOf.map((pool) => <li key={pool.id}><a href="#/settings/pools">{pool.name}</a></li>)}</ul>}</dd>
      </dl>
      {subscription && account.status === "signing_in" && !signingIn &&
        <span><Button icon="x" pending={cancelling} pendingLabel="Cancelling..." onClick={() => void cancel()}>Cancel sign-in</Button></span>}
      {subscription && account.status !== "signed_in" && account.status !== "signing_in" && !signingIn &&
        <span><Button variant="primary" icon="log-in" onClick={() => setSigningIn(true)}>Sign in again</Button></span>}
      {canSignOut(account) && !signingIn && inUse === undefined &&
        <span><Button icon="log-out" pending={signingOut} pendingLabel="Signing out..." onClick={() => void signOutAccount(false)}>Sign out</Button></span>}
      {inUse !== undefined && <InUseNotice used={inUse} projects={session.projects.kind === "ok" ? session.projects.value : undefined}
        pending={signingOut} onStop={() => void signOutAccount(true)} />}
      {subscription && signingIn && <SignInAgain account={account} onSignedIn={() => { onChanged({ ...account, status: "signed_in", status_label: "Signed in" }); }} />}
      {account.method === "url" && <div className="stack-small">
        <span><Button icon="refresh-cw" pending={checking} pendingLabel="Checking..." onClick={() => void check()}>Check again</Button></span>
        {checkLine && <p role="status">{checkLine}</p>}
      </div>}
      {error && <Notice tone="danger" alert>{error}</Notice>}
      <span><Button variant="quiet" icon="copy" onClick={() => {
        void navigator.clipboard?.writeText(supportDetails(account)).then(() => session.showToast("Details copied."), () => undefined);
      }}>Copy details for support</Button></span>
    </Drawer>
  </>;
}
