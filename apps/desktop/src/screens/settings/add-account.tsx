import { useCallback, useEffect, useRef, useState } from "react";
import {
  accountCatalog, addAccount, checkAccount, localModels, poolSummaries, savePool, POOL_MEMBER_MAX,
  type AccountProvider, type AccountView, type ProviderEntry, type Result, type SignInMethodEntry,
} from "../../api";
import type { NativeAuthMethod } from "../../native-auth";
import { useAppSession, useLoad } from "../../state";
import { Button, ConfirmDialog, Dialog, Icon, LoadError, NotReady, Notice, ProviderMark, SecretField, Select, Skeleton, TextField } from "../../ui";
import { readyAdded } from "./pools";
import { discardAccount, SignInPanel, signInRunning, signInStage, useSignIn, type SubscriptionProvider } from "./sign-in";

export const TILE_LINES: Record<AccountProvider, string> = {
  chatgpt: "Use your ChatGPT plan or an OpenAI key with Codex.",
  claude: "Use your Claude subscription or an Anthropic key with Claude Code.",
  openrouter: "One key for many models.",
  openai_api: "Pay per use with an OpenAI API key.",
  anthropic_api: "Pay per use with an Anthropic API key.",
  openai_compatible: "Any server that speaks the OpenAI API.",
  local: "Ollama or another model running on this Mac.",
};

const METHOD_DETAILS: Record<string, string> = {
  "chatgpt:subscription": "Uses your ChatGPT plan.",
  "chatgpt:api_key": "Pay per use with an OpenAI key.",
  "chatgpt:existing": "Reuses the account Codex is already signed in to.",
  "claude:subscription": "Uses your Pro or Max plan.",
  "claude:api_key": "Pay per use with an Anthropic key.",
};

/** One way to sign in, as step 2 lists it. `native` is the managed sign-in it starts. */
export interface MethodOption {
  key: string;
  title: string;
  detail: string;
  recommended: boolean;
  entry: SignInMethodEntry;
  native?: NativeAuthMethod;
}

/** Lists a provider's sign-in methods in catalog order, the first marked Recommended. ChatGPT's
 * subscription also offers reusing the Codex sign-in on this Mac.
 * Input: the provider entry. Output: the options. */
export function methodOptions(provider: ProviderEntry): MethodOption[] {
  const options: MethodOption[] = [];
  for (const entry of provider.methods) {
    const native: NativeAuthMethod | undefined = entry.method !== "subscription" ? undefined
      : provider.provider === "chatgpt" ? "codex_device_code" : provider.provider === "claude" ? "claude_official" : undefined;
    options.push({ key: `${provider.provider}:${entry.method}`, title: entry.label, detail: METHOD_DETAILS[`${provider.provider}:${entry.method}`] ?? "",
      recommended: options.length === 0 && provider.methods.length > 1, entry, native });
    if (provider.provider === "chatgpt" && entry.method === "subscription") {
      options.push({ key: "chatgpt:existing", title: "Use the Codex sign-in on this Mac", detail: METHOD_DETAILS["chatgpt:existing"]!,
        recommended: false, entry, native: "codex_existing" });
    }
  }
  return options;
}

/** Proposes an unused account name such as "ChatGPT 3". Inputs: the provider and the
 * existing accounts. Output: "<short name> <n>" with the lowest n not already taken. */
export function defaultAccountName(provider: ProviderEntry, accounts: AccountView[]): string {
  const short = provider.name.split(" / ")[0]!.replace(/ \(.*\)$/, "");
  const taken = new Set(accounts.map((account) => account.name));
  let index = 1;
  while (taken.has(`${short} ${index}`)) index += 1;
  return `${short} ${index}`;
}

/** Checks a base URL the way the engine will: https anywhere, or http only on this Mac.
 * Input: the text. Output: an error sentence, or undefined when it is acceptable. */
export function urlError(text: string): string | undefined {
  const message = "Enter a full URL that starts with https://, or http://127.0.0.1 for this Mac.";
  try {
    const url = new URL(text.trim());
    if (url.username || url.password) return message;
    if (url.protocol === "https:") return undefined;
    return url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) ? undefined : message;
  } catch {
    return message;
  }
}

/** Checks the account name. Input: the text. Output: an error sentence or undefined. */
export function nameError(text: string): string | undefined {
  const name = text.trim();
  return !name || [...name].length > 60 || /[\u0000-\u001f\u007f-\u009f]/u.test(name) ? "Use an account name of up to 60 characters on one line." : undefined;
}

/** The Account added line for a method. Inputs: the method entry, the saved account and, for
 * URL accounts, the check result. Output: the line. */
export function addedLine(entry: SignInMethodEntry, account: AccountView, check?: Result<{ models: string[] }>): string {
  if (entry.method === "api_key") return "Key saved. CrazyAgent will check it the first time an agent uses it.";
  if (entry.method === "subscription") return account.status === "signed_in" ? "Signed in." : "Saved. Sign in from the account when you're ready.";
  if (!check) return "Saved. Checking the connection...";
  if (check.kind !== "ok") return check.kind === "not_ready" ? "Saved." : `Saved. ${check.message}`;
  const count = check.value.models.length;
  return `Connected. ${count} ${count === 1 ? "model" : "models"} found.`;
}

type Step =
  | { kind: "provider" }
  | { kind: "method"; provider: ProviderEntry }
  | { kind: "form"; provider: ProviderEntry; option: MethodOption }
  | { kind: "done"; provider: ProviderEntry; option: MethodOption; account: AccountView; line: string };

function stepTitle(step: Step): string {
  return step.kind === "provider" ? "Add an account" : step.kind === "method" ? "How do you want to sign in?"
    : step.kind === "done" ? "Account added" : step.option.title;
}

/** Step 1 of Add account: one tile per catalog provider with its name and plain line.
 * Inputs: the catalog and the pick handler. Output: the tiles, with no secret entry. */
export function ProviderTiles({ catalog, onPick }: { catalog: ProviderEntry[]; onPick: (provider: ProviderEntry) => void }) {
  return <div className="tiles">
    {catalog.map((provider) => <button type="button" key={provider.provider} className="tile" onClick={() => onPick(provider)}>
      <ProviderMark provider={provider.provider} name={provider.name} large />
      <span className="title">{provider.name}</span>
      <span className="detail">{TILE_LINES[provider.provider]}</span>
    </button>)}
  </div>;
}

/** Step 2 of Add account: the provider's sign-in methods from `methodOptions`.
 * Inputs: the provider and the pick handler. Output: one button per method. */
export function MethodMenu({ provider, onPick }: { provider: ProviderEntry; onPick: (option: MethodOption) => void }) {
  return <div className="methods">
    {methodOptions(provider).map((option) => <button type="button" key={option.key} className="method" onClick={() => onPick(option)}>
      <Icon name={option.native ? "log-in" : option.entry.method === "url" ? "link" : "key-round"} />
      <span className="stack-small">
        <span className="title">{option.title}{option.recommended && <span className="badge tone-info">Recommended</span>}</span>
        {option.detail && <span className="small secondary">{option.detail}</span>}
      </span>
      <Icon name="chevron-right" />
    </button>)}
  </div>;
}

function SubscriptionForm({ provider, option, name, onName, onAdded, onCreated, onUseKey, onRunning }: {
  provider: ProviderEntry;
  option: MethodOption;
  name: string;
  onName: (name: string) => void;
  onAdded: (account: AccountView, line: string) => void;
  onUseKey: () => void;
  onRunning: (stop: (() => void) | undefined) => void;
  onCreated: (account: AccountView) => void;
}) {
  const session = useAppSession();
  const [account, setAccount] = useState<AccountView>();
  const [error, setError] = useState<string>();
  const [queued, setQueued] = useState<NativeAuthMethod>();
  const signIn = useSignIn(account?.id);
  const subscription = provider.provider as SubscriptionProvider;
  const stage = signInStage(signIn.state, subscription);
  const reported = useRef(false);
  const running = signInRunning(signIn.state);
  const cancel = signIn.cancel;

  useEffect(() => {
    onRunning(running ? () => void cancel() : undefined);
  }, [running, cancel, onRunning]);
  useEffect(() => () => onRunning(undefined), [onRunning]);

  useEffect(() => {
    if (!queued || !signIn.state?.valid) return;
    setQueued(undefined);
    void signIn.start(queued);
  }, [queued, signIn]);

  useEffect(() => {
    if (!account || stage.kind !== "signed_in" || reported.current) return;
    reported.current = true;
    onAdded({ ...account, status: "signed_in", status_label: "Signed in" }, stage.line);
  }, [account, stage, onAdded]);

  const begin = async (method: NativeAuthMethod) => {
    if (account) return void signIn.start(method);
    const invalid = nameError(name);
    if (invalid) return setError(invalid);
    const added = session.noteResult(await addAccount({ provider: provider.provider, method: "subscription", name: name.trim() }));
    if (added.kind !== "ok") return setError(added.kind === "not_ready" ? "Adding accounts isn't ready yet in this build." : added.message);
    setError(undefined);
    setAccount(added.value);
    onCreated(added.value);
    setQueued(method);
  };

  return <div className="stack">
    <TextField id="account-name" label="Account name" value={name} onValue={onName} maxLength={60} error={error} disabled={Boolean(account)} />
    <SignInPanel provider={subscription} method={option.native!} state={account ? signIn.state : undefined}
      onStart={(method) => void begin(method)} onCancel={() => void signIn.cancel()} onCheck={() => void signIn.check()}
      onClaudeCode={signIn.sendClaudeCode} onUseKey={onUseKey} idle={!account} />
  </div>;
}

/** The key or URL form: name, base URL, masked key and model as the method entry asks.
 * The key stays in an uncontrolled field and is sent once. Inputs: the provider, method,
 * name and handlers. Output: the form; saving calls the engine through the app session. */
export function KeyOrUrlForm({ provider, option, name, onName, onAdded }: {
  provider: ProviderEntry;
  option: MethodOption;
  name: string;
  onName: (name: string) => void;
  onAdded: (account: AccountView) => void;
}) {
  const session = useAppSession();
  const entry = option.entry;
  const key = useRef<HTMLInputElement>(null);
  const [url, setUrl] = useState(entry.url_preset ?? "");
  const [model, setModel] = useState("");
  const [models, setModels] = useState<Result<string[]>>();
  const [errors, setErrors] = useState<{ name?: string; url?: string; key?: string; model?: string; form?: string }>({});
  const [pending, setPending] = useState(false);
  const local = provider.provider === "local";

  const findModels = async () => {
    const invalid = urlError(url);
    if (invalid) return setErrors({ url: invalid });
    setModels(undefined);
    const found = await localModels(url.trim());
    setModels(found);
    if (found.kind === "ok" && found.value.length > 0 && !model) setModel(found.value[0]!);
  };

  const save = async () => {
    const next = {
      name: nameError(name),
      url: entry.url === "hidden" ? undefined : entry.url === "optional" && !url.trim() ? undefined : urlError(url),
      model: entry.model === "required" && !model.trim() ? "Choose a model." : model.trim() && /\s/.test(model.trim()) ? "Use a model name without spaces." : undefined,
      key: entry.key === "required" && !key.current?.value ? "Enter an API key." : undefined,
    };
    setErrors(next);
    if (next.name || next.url || next.model || next.key) return;
    let secret = entry.key === "hidden" ? "" : key.current?.value ?? "";
    if (key.current) key.current.value = "";
    setPending(true);
    try {
      const sending = addAccount({ provider: provider.provider, method: entry.method, name: name.trim(),
        url: entry.url === "hidden" ? null : url.trim() || null, model: entry.model === "hidden" ? null : model.trim() || null }, secret || undefined);
      secret = "";
      const added = session.noteResult(await sending);
      if (added.kind === "ok") onAdded(added.value);
      else setErrors({ form: added.kind === "not_ready" ? "Adding accounts isn't ready yet in this build." : added.message });
    } finally {
      secret = "";
      setPending(false);
    }
  };

  const modelList = models?.kind === "ok" && models.value.length > 0 ? models.value : undefined;
  return <form className="stack" onSubmit={(event) => { event.preventDefault(); void save(); }}>
    <TextField id="account-name" label="Account name" value={name} onValue={onName} maxLength={60} error={errors.name} />
    {entry.url !== "hidden" && <TextField id="account-url" label="Base URL" type="url" value={url} onValue={setUrl} error={errors.url}
      helper={local ? "Ollama listens on http://127.0.0.1:11434/v1 by default." : "For example https://api.together.xyz/v1"} spellCheck={false} />}
    {entry.key !== "hidden" && <SecretField id="account-key" label={entry.key === "optional" ? "API key (optional)" : "API key"} inputRef={key} error={errors.key}
      helper="Sent once to CrazyAgent's engine, which keeps it privately. You won't see it again." />}
    {entry.model !== "hidden" && (modelList
      ? <Select id="account-model" label={local ? "Model" : "Default model"} value={model} onValue={setModel} error={errors.model}
        options={modelList.map((value) => ({ value, label: value }))} />
      : <TextField id="account-model" label={local || entry.method === "url" ? "Model" : "Default model"} value={model} onValue={setModel} error={errors.model}
        helper={entry.model === "optional" ? "Optional. Leave empty to use the provider's default." : undefined} spellCheck={false} />)}
    {local && <div className="row">
      <Button icon="search" onClick={() => void findModels()}>Find models</Button>
      {models?.kind === "ok" && models.value.length === 0 && <span className="secondary">No models found. Pull one in Ollama, then try again.</span>}
    </div>}
    {models && models.kind !== "ok" && <Notice tone="danger" alert>{models.kind === "not_ready" ? "Finding models isn't ready yet in this build. Type the model name instead."
      : models.code === "account_url_unreachable" ? `Can't reach Ollama at ${url.trim()}. Start Ollama, then try again.` : models.message}</Notice>}
    {errors.form && <Notice tone="danger" alert>{errors.form}</Notice>}
    <div className="row"><Button type="submit" variant="primary" pending={pending} pendingLabel="Saving...">{entry.method === "api_key" ? "Save key" : "Save"}</Button></div>
  </form>;
}

function AddToPool({ account }: { account: AccountView }) {
  const session = useAppSession();
  const [pools] = useLoad(poolSummaries);
  const [chosen, setChosen] = useState("");
  const [state, setState] = useState<{ kind: "idle" | "saving" } | { kind: "added"; note?: string } | { kind: "error"; message: string }>({ kind: "idle" });
  if (pools.kind === "loading") return <Skeleton shape="line" />;
  if (pools.kind === "not_ready") return <NotReady what="Pools" compact />;
  if (pools.kind === "error") return <Notice tone="danger">Couldn't load pools.</Notice>;
  if (pools.value.length === 0) return <p className="secondary">No pools yet. <a href="#/settings/pools?action=new">New pool</a></p>;
  const options = pools.value.map((pool) => ({ value: pool.id, label: pool.name,
    disabledReason: pool.members.length >= POOL_MEMBER_MAX ? "full, 15 of 15" : pool.members.some((member) => member.id === account.id) ? "already in it" : undefined }));
  const target = pools.value.find((pool) => pool.id === chosen);
  if (state.kind === "added") return <div className="stack-small">
    <p className="live" role="status"><Icon name="check" />Added to {target?.name}.</p>
    {state.note && <Notice tone="warn" alert>{state.note}</Notice>}
  </div>;
  return <div className="row">
    <Select id="add-to-pool" label="Pool" value={chosen} onValue={setChosen} options={[{ value: "", label: "Choose a pool" }, ...options]} />
    <Button disabled={!target} pending={state.kind === "saving"} pendingLabel="Adding..." onClick={async () => {
      if (!target) return;
      setState({ kind: "saving" });
      const listed = { name: target.name, members: target.members.map((member) => member.id) };
      const members = [...listed.members, account.id];
      const saved = session.noteResult(await savePool(target.id, target.name, members, listed));
      if (saved.kind !== "ok") return setState({ kind: "error", message: saved.kind === "not_ready" ? "Not ready yet." : saved.message });
      setState({ kind: "added", note: await readyAdded(listed.members, members, [account], session.noteResult) });
    }}>Add to pool</Button>
    {state.kind === "error" && <Notice tone="danger" alert>{state.message}</Notice>}
  </div>;
}

function Done({ account, line }: { account: AccountView; line: string }) {
  const [pool, setPool] = useState(false);
  return <div className="stack">
    <div className="success" role="status"><Icon name="circle-check" size={28} /><p><strong>{account.name}</strong></p><p>{line}</p></div>
    {pool ? <AddToPool account={account} /> : <span><Button icon="layers" onClick={() => setPool(true)}>Add to a pool</Button></span>}
  </div>;
}

/** The Add account dialog: provider tiles, sign-in method, the method's form or managed
 * sign-in, then Account added. A subscription account the owner leaves before it is signed in
 * (close, Back, Stop signing in, a key instead) has its sign-in cancelled and is removed.
 * Inputs: open flag, the current accounts (for default names), close handler and a handler
 * called after each added or removed account. */
export function AddAccountDialog({ open, accounts, onClose, onAdded }: {
  open: boolean;
  accounts: AccountView[];
  onClose: () => void;
  onAdded: () => void;
}) {
  const [catalog, reloadCatalog] = useLoad(accountCatalog);
  const [step, setStep] = useState<Step>({ kind: "provider" });
  const [name, setName] = useState("");
  const [confirmStop, setConfirmStop] = useState(false);
  const session = useAppSession();
  const stop = useRef<(() => void) | undefined>(undefined);
  const unfinished = useRef<AccountView | undefined>(undefined);
  const onRunning = useCallback((value: (() => void) | undefined) => { stop.current = value; }, []);

  const leave = () => {
    const left = unfinished.current;
    unfinished.current = undefined;
    stop.current = undefined;
    if (!left) return;
    void discardAccount(left, session.noteResult).then((line) => { session.showToast(line); onAdded(); });
  };
  const reset = () => { leave(); setStep({ kind: "provider" }); setName(""); };
  const close = () => {
    if (stop.current) return setConfirmStop(true);
    reset();
    onClose();
  };
  const pickMethod = (provider: ProviderEntry, option: MethodOption) => {
    setName(defaultAccountName(provider, accounts));
    setStep({ kind: "form", provider, option });
  };
  const pickProvider = (provider: ProviderEntry) => {
    const options = methodOptions(provider);
    if (options.length === 1) pickMethod(provider, options[0]!);
    else setStep({ kind: "method", provider });
  };
  const back = step.kind === "method" ? () => setStep({ kind: "provider" })
    : step.kind === "form" ? () => { leave(); if (methodOptions(step.provider).length > 1) setStep({ kind: "method", provider: step.provider }); else setStep({ kind: "provider" }); }
    : undefined;
  const added = (account: AccountView, line: string) => {
    if (step.kind !== "form") return;
    stop.current = undefined;
    unfinished.current = undefined;
    setStep({ kind: "done", provider: step.provider, option: step.option, account, line });
    onAdded();
  };

  const checking = step.kind === "done" && step.option.entry.method === "url" ? step.account.id : undefined;
  useEffect(() => {
    if (!checking) return;
    let live = true;
    void checkAccount(checking).then((check) => {
      if (live) setStep((current) => current.kind === "done" && current.account.id === checking
        ? { ...current, line: addedLine(current.option.entry, current.account, check) } : current);
    });
    return () => { live = false; };
  }, [checking]);

  let body;
  if (step.kind === "provider") {
    body = catalog.kind === "loading" ? <Skeleton shape="card" count={4} /> : catalog.kind === "not_ready" ? <NotReady what="Adding accounts" compact />
      : catalog.kind === "error" ? <LoadError message="Couldn't load the account types." onRetry={reloadCatalog} />
      : <ProviderTiles catalog={catalog.value} onPick={pickProvider} />;
  } else if (step.kind === "method") {
    body = <MethodMenu provider={step.provider} onPick={(option) => pickMethod(step.provider, option)} />;
  } else if (step.kind === "form") {
    body = step.option.native
      ? <SubscriptionForm key={step.option.key} provider={step.provider} option={step.option} name={name} onName={setName}
        onAdded={added} onRunning={onRunning} onCreated={(account) => { unfinished.current = account; }} onUseKey={() => {
          leave();
          const keyOption = methodOptions(step.provider).find((option) => option.entry.method === "api_key");
          if (keyOption) pickMethod(step.provider, keyOption);
        }} />
      : <KeyOrUrlForm key={step.option.key} provider={step.provider} option={step.option} name={name} onName={setName}
        onAdded={(account) => added(account, addedLine(step.option.entry, account))} />;
  } else {
    body = <Done account={step.account} line={step.line} />;
  }

  return <>
    <Dialog open={open} title={stepTitle(step)} onClose={close} onBack={back}
      footer={step.kind === "done" ? <><Button icon="plus" onClick={reset}>Add another account</Button><Button variant="primary" onClick={close}>Done</Button></> : undefined}>
      {body}
    </Dialog>
    <ConfirmDialog open={confirmStop} title="Stop signing in?" confirmLabel="Stop signing in" cancelLabel="Keep signing in"
      onCancel={() => setConfirmStop(false)} onConfirm={() => { setConfirmStop(false); reset(); onClose(); }}>
      The sign-in is cancelled and the account you started is removed, since it isn't signed in.
    </ConfirmDialog>
  </>;
}
