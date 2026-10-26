import { useEffect, useRef, useState } from "react";
import { checkAccount, renameAccount, type AccountView, type PoolSummary } from "../../api";
import { useAppSession } from "../../state";
import { Button, Drawer, Notice, ProviderMark, StatusBadge, TextField } from "../../ui";
import { nameError } from "./add-account";
import { cancelSignIn, SignInPanel, signInStage, useSignIn, type SubscriptionProvider } from "./sign-in";

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
 * Check again), model, pools, support copy and Delete account. Inputs: the account (or
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
  const [error, setError] = useState<string>();
  const accountId = account?.id;
  useEffect(() => {
    setName(account?.name ?? "");
    setNameMessage(undefined);
    setSigningIn(signIn);
    setCheckLine(undefined);
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
