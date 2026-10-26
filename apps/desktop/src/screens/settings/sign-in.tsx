import { useCallback, useEffect, useRef, useState } from "react";
import { nextGeneration, plainError, removeAccount, signInRecord, type AccountView, type Result } from "../../api";
import { cancelNativeAuth, logoutNativeAuth, readNativeAuth, startNativeAuth, submitClaudeAuthCode } from "../../engine-client";
import {
  nativeAuthAllowed, nativeAuthFailureCode, nativeAuthText, nativeClaudeInputAllowed, reduceNativeAuth, validateClaudeAuthCode,
  type NativeAuthCommand, type NativeAuthMethod, type NativeAuthState, type NativeAuthView,
} from "../../native-auth";
import { Button, Icon, Notice, SecretField } from "../../ui";

type Challenge = NonNullable<NativeAuthView["challenge"]>;
type Account = NonNullable<NativeAuthView["account"]>;
export type SubscriptionProvider = "chatgpt" | "claude";
export type FailureHint = "retry" | "check" | "expired" | "missing";

/** What the sign-in panel shows for one native sign-in state. */
export type SignInStage =
  | { kind: "loading" }
  | { kind: "start" }
  | { kind: "working"; text: string }
  | { kind: "waiting"; challenge: Challenge | null; claudeInput: boolean; codeSent: boolean }
  | { kind: "signed_in"; line: string }
  | { kind: "failed"; text: string; hint: FailureHint };

const ACTIVE_PHASES = ["starting", "awaiting_owner", "checking_account", "cancelling"];
const EXPIRED = ["native_auth_sign_in_expired", "native_auth_claude_deadline", "native_auth_bootstrap_timeout"];
const CHECK = ["native_auth_outcome_unknown_read_same_operation", "native_auth_claude_input_outcome_unknown"];
export const POLL_MS = 2000;

/** Turns a sign-in failure code into one plain sentence and the action that fits it.
 * Inputs: the code and the provider. Output: the copy and its hint. */
export function signInFailure(code: string, provider: SubscriptionProvider): { text: string; hint: FailureHint } {
  if (code === "native_auth_host_unavailable") {
    const [harness, name] = provider === "chatgpt" ? ["Codex", "ChatGPT"] : ["Claude Code", "Claude"];
    return { text: `${harness} isn't installed on this Mac, so ${name} sign-in isn't available. Install ${harness}, then check again in Settings, Harnesses.`, hint: "missing" };
  }
  if (EXPIRED.includes(code)) return { text: "This code expired.", hint: "expired" };
  if (CHECK.includes(code)) return { text: plainError(code), hint: "check" };
  if (code === "native_auth_cancelled") return { text: "Sign-in cancelled.", hint: "retry" };
  if (code === "native_auth_authentication_failed") return { text: "Sign-in didn't finish.", hint: "retry" };
  const known = plainError(code);
  return { text: known === plainError("") ? "Sign-in didn't finish. Try again." : known, hint: "retry" };
}

const STUCK = new Set(["native_auth_operation_missing", "native_profile_source_conflict", "native_auth_operation_scope_conflict", "native_auth_recovery_required"]);

/** The sentence when a sign-in could not be cancelled. Input: the refusal code. Output: the
 * sentence, or undefined when the sign-in had already stopped. */
export function cancelError(code: string): string | undefined {
  if (code === "native_auth_not_active") return undefined;
  if (STUCK.has(code)) return "CrazyAgent can't stop this sign-in from here. Delete the account to clear it, then add it again.";
  return plainError(code);
}

const SETTLE_TRIES = 20;
const SETTLE_MS = 500;

/** Cancels the running sign-in of one account, outside any open sign-in panel: reads its
 * sign-in record, asks the engine to cancel that operation and waits up to 10 s for it to
 * stop, since the app refuses other changes until it has. Input: the account id. Output: ok
 * once stopped or already stopped, otherwise the refusal in plain words. */
export async function cancelSignIn(accountId: string): Promise<Result<void>> {
  const record = await signInRecord(accountId);
  if (record.kind !== "ok") return record;
  try {
    let view = await cancelNativeAuth(record.value.ticket);
    for (let tries = 0; ACTIVE_PHASES.includes(view.phase); tries += 1) {
      if (tries === SETTLE_TRIES) return { kind: "error", code: "native_auth_cancel_pending", message: plainError("native_auth_cancel_pending") };
      await new Promise((resolve) => setTimeout(resolve, SETTLE_MS));
      const again = await signInRecord(accountId);
      if (again.kind !== "ok") return again;
      view = await readNativeAuth(again.value.ticket);
    }
    return { kind: "ok", value: undefined };
  } catch (failure) {
    const code = nativeAuthFailureCode(failure);
    const message = cancelError(code);
    return message === undefined ? { kind: "ok", value: undefined } : { kind: "error", code, message };
  }
}

/** Signs one subscription account out: reads its sign-in record, asks the engine to log the
 * harness out and waits up to 10 s for the engine to see it signed out. Input: the account
 * id. Output: ok once signed out, otherwise the refusal in plain words, such as
 * `native_profile_in_use` while an agent uses the account. */
export async function signOut(accountId: string): Promise<Result<void>> {
  const record = await signInRecord(accountId);
  if (record.kind !== "ok") return record;
  try {
    let view = await logoutNativeAuth(record.value.ticket);
    for (let tries = 0; ACTIVE_PHASES.includes(view.phase); tries += 1) {
      if (tries === SETTLE_TRIES) break;
      await new Promise((resolve) => setTimeout(resolve, SETTLE_MS));
      const again = await signInRecord(accountId);
      if (again.kind !== "ok") return again;
      view = await readNativeAuth(again.value.ticket);
    }
    if (view.phase === "unauthenticated_observed") return { kind: "ok", value: undefined };
    const code = view.code ?? "native_auth_outcome_unknown_read_same_operation";
    return { kind: "error", code, message: plainError(code) };
  } catch (failure) {
    const code = nativeAuthFailureCode(failure);
    return { kind: "error", code, message: plainError(code) };
  }
}

/** Removes an account Add account created but never signed in, after cancelling its sign-in.
 * Inputs: the account and the session's result recorder. Output: the line to show. */
export async function discardAccount(account: AccountView, note: <T>(result: Result<T>) => Result<T>): Promise<string> {
  await cancelSignIn(account.id);
  const removed = note(await removeAccount(account.id));
  return removed.kind === "ok" ? `${account.name} wasn't signed in, so it was removed.`
    : `Couldn't remove the unfinished account ${account.name}. It stays in your list as Not signed in. Delete it there.`;
}

function capitalized(text: string): string {
  return text ? text[0]!.toUpperCase() + text.slice(1) : text;
}

/** The line shown once a subscription is signed in, built only from what the engine observed.
 * Input: the observed account or null. Output: "Signed in as ... (ChatGPT Plus)." or similar. */
export function signedInLine(account: Account | null | undefined): string {
  if (!account) return "Signed in.";
  if (account.kind === "codex_api_key") return "Signed in with an OpenAI API key.";
  if (account.kind === "codex_amazon_bedrock") return "Signed in with Amazon Bedrock.";
  const [email, plan] = account.kind === "codex_chatgpt"
    ? [account.email, account.plan_type ? `ChatGPT ${capitalized(account.plan_type)}` : ""]
    : [account.email.state === "value" ? account.email.text : null,
      account.subscription_type.state === "value" ? `Claude ${capitalized(account.subscription_type.text)}` : ""];
  const who = email ? `Signed in as ${nativeAuthText(email)}` : "Signed in";
  return plan ? `${who} (${nativeAuthText(plan)}).` : `${who}.`;
}

/** Maps the native sign-in state to what the panel shows. Pure; the reducer in native-auth.ts
 * stays the authority on which replies count. Inputs: state and provider. Output: the stage. */
export function signInStage(state: NativeAuthState | undefined, provider: SubscriptionProvider): SignInStage {
  if (!state) return { kind: "loading" };
  if (state.claude_input?.status === "sending") return { kind: "working", text: "Sending the code..." };
  const flight = state.in_flight?.action;
  if (flight) {
    const text = flight === "codex_device_code" ? "Getting a sign-in code..." : flight === "read" ? "Checking..."
      : flight === "cancel" ? "Cancelling sign-in..." : flight === "logout" ? "Signing out..." : "Starting sign-in...";
    return { kind: "working", text };
  }
  if (state.error) {
    const failure = signInFailure(state.error, provider);
    return { kind: "failed", text: failure.text, hint: state.recover ? "check" : failure.hint };
  }
  const operation = state.operation;
  if (!operation) return state.recover ? { kind: "failed", ...signInFailure(CHECK[0]!, provider) } : { kind: "start" };
  switch (operation.phase) {
    case "starting": return { kind: "working", text: "Starting sign-in..." };
    case "checking_account": return { kind: "working", text: "Checking your account..." };
    case "cancelling": return { kind: "working", text: "Cancelling sign-in..." };
    case "awaiting_owner": return { kind: "waiting", challenge: state.presentation?.challenge ?? null,
      claudeInput: nativeClaudeInputAllowed(state, false), codeSent: state.claude_input?.status === "accepted" };
    case "authenticated_observed": return operation.action.kind === "logout" ? { kind: "start" }
      : { kind: "signed_in", line: signedInLine(state.presentation?.account) };
    case "uncertain": return { kind: "failed", text: plainError("native_auth_recovery_required"), hint: "check" };
    default: return { kind: "failed", ...signInFailure(operation.code ?? (operation.phase === "unavailable" ? "native_auth_host_unavailable"
      : operation.phase === "cancelled" ? "native_auth_cancelled" : "native_auth_authentication_failed"), provider) };
  }
}

/** Whether a sign-in is still running and worth polling or confirming before close.
 * Input: the state. Output: true while the engine holds an unfinished operation. */
export function signInRunning(state: NativeAuthState | undefined): boolean {
  return Boolean(state?.in_flight || state?.operation && ACTIVE_PHASES.includes(state.operation.phase));
}

/** Runs managed sign-in for one subscription account: reads its sign-in record, starts,
 * polls, cancels and sends the Claude code through the native-auth reducer.
 * Input: the account id. Output: the state and the actions. */
export function useSignIn(accountId: string | undefined) {
  const [state, setState] = useState<NativeAuthState>();
  const current = useRef<NativeAuthState | undefined>(undefined);
  const navigation = useRef(0);
  const update = useCallback((event: Parameters<typeof reduceNativeAuth>[1]) => {
    current.current = reduceNativeAuth(current.current, event);
    setState(current.current);
  }, []);

  const act = useCallback(async (action: Exclude<NativeAuthCommand, "logout">) => {
    const selected = current.current;
    if (!selected || !nativeAuthAllowed(selected, action)) return;
    const turn = selected.context.navigation;
    const request = nextGeneration();
    update({ kind: "begin", action, request });
    if (current.current?.in_flight?.request !== request) return;
    const ticket = current.current.context.ticket;
    try {
      const reply = action === "read" ? await readNativeAuth(ticket) : action === "cancel" ? await cancelNativeAuth(ticket)
        : await startNativeAuth(ticket, action);
      update({ kind: "reply", navigation: turn, request, reply });
    } catch (failure) {
      update({ kind: "failure", navigation: turn, request, code: nativeAuthFailureCode(failure) });
    }
  }, [update]);

  const open = useCallback(async () => {
    if (!accountId) return;
    const turn = navigation.current;
    const record = await signInRecord(accountId);
    if (turn !== navigation.current) return;
    if (record.kind !== "ok") {
      current.current = { open: true, valid: false, confirming_logout: false, error: record.kind === "not_ready" ? "unavailable" : record.code,
        context: { ticket: "", connection: accountId, config_id: "", source_hash: "", navigation: turn, harness: "" } };
      setState(current.current);
      return;
    }
    update({ kind: "open", detail: record.value, navigation: turn });
    if (current.current?.recover) await act("read");
  }, [accountId, act, update]);

  useEffect(() => {
    navigation.current += 1;
    current.current = undefined;
    setState(undefined);
    void open();
    return () => { navigation.current += 1; };
  }, [open]);

  useEffect(() => {
    if (!state || state.in_flight || !state.valid || !signInRunning(state)) return;
    const timer = setTimeout(() => void act("read"), POLL_MS);
    return () => clearTimeout(timer);
  }, [state, act]);

  const start = useCallback(async (method: NativeAuthMethod) => {
    if (!current.current?.valid) await open();
    await act(method);
  }, [act, open]);

  const sendClaudeCode = useCallback(async (code: string) => {
    try {
      validateClaudeAuthCode(code);
      const selected = current.current;
      if (!selected || !nativeClaudeInputAllowed(selected, false)) return;
      const turn = selected.context.navigation;
      const ticket = selected.context.ticket;
      const request = nextGeneration();
      update({ kind: "claude_input_begin", request });
      if (current.current?.claude_input?.request !== request) return;
      try {
        const delivery = submitClaudeAuthCode(ticket, code);
        code = "";
        await delivery;
        update({ kind: "claude_input_accepted", navigation: turn, request });
      } catch {
        update({ kind: "claude_input_failure", navigation: turn, request });
      }
    } finally {
      code = "";
    }
  }, [update]);

  const check = useCallback(async () => {
    if (!current.current?.valid) await open();
    if (current.current?.valid && !current.current.recover) await act("read");
  }, [act, open]);

  return { state, start, cancel: () => act("cancel"), check, sendClaudeCode, reopen: open };
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return <Button size="small" icon={copied ? "check" : "copy"} onClick={() => {
    void navigator.clipboard?.writeText(text).then(() => setCopied(true), () => setCopied(false));
  }}>{copied ? "Copied" : label}</Button>;
}

function ChallengeView({ challenge }: { challenge: Challenge }) {
  const url = challenge.kind === "codex_device_code" ? challenge.verification_url : challenge.auth_url;
  return <div className="stack-small">
    {challenge.kind === "codex_device_code" && <>
      <p>Enter this code on the sign-in page:</p>
      <div className="device-code" aria-label="Sign-in code">{nativeAuthText(challenge.user_code)}</div>
      <span><CopyButton text={challenge.user_code} label="Copy code" /></span>
    </>}
    <p className="small secondary">{challenge.kind === "codex_device_code" ? "Sign-in page" : "Open this page in your browser to sign in"}</p>
    <p className="mono small sign-in-url">{nativeAuthText(url)}</p>
    <span><CopyButton text={url} label="Copy link" /></span>
  </div>;
}

function ClaudeCode({ onSend }: { onSend: (code: string) => Promise<void> }) {
  const input = useRef<HTMLInputElement>(null);
  const [invalid, setInvalid] = useState(false);
  useEffect(() => {
    const element = input.current;
    return () => { if (element) element.value = ""; };
  }, []);
  return <form className="form" onSubmit={async (event) => {
    event.preventDefault();
    let code = input.current?.value ?? "";
    if (input.current) input.current.value = "";
    setInvalid(false);
    try {
      validateClaudeAuthCode(code);
      const sending = onSend(code);
      code = "";
      await sending;
    } catch {
      setInvalid(true);
    } finally {
      code = "";
    }
  }}>
    <SecretField id="claude-sign-in-code" label="Paste the code from the Claude page" inputRef={input}
      helper="Only if Claude shows you a code. It's sent once and not kept." error={invalid ? plainError("native_auth_claude_input_invalid") : undefined} />
    <span><Button type="submit" variant="primary">Send code</Button></span>
  </form>;
}

/** The managed sign-in panel for one subscription account. Pure on its props, so tests
 * render it from real reducer states. Inputs: provider, method, state, the actions, and idle
 * (no account yet, so it shows the start buttons). */
export function SignInPanel({ provider, method, state, onStart, onCancel, onCheck, onClaudeCode, onUseKey, idle = false }: {
  provider: SubscriptionProvider;
  method: NativeAuthMethod;
  state: NativeAuthState | undefined;
  onStart: (method: NativeAuthMethod) => void;
  onCancel: () => void;
  onCheck: () => void;
  onClaudeCode: (code: string) => Promise<void>;
  onUseKey?: () => void;
  idle?: boolean;
}) {
  if (!idle && state && !state.open) return null;
  const stage: SignInStage = idle ? { kind: "start" } : signInStage(state, provider);
  if (stage.kind === "loading") return <p className="live" role="status"><Icon name="loader-circle" spin />Getting ready to sign in...</p>;
  if (stage.kind === "working") return <div className="stack-small">
    <p className="live" role="status"><Icon name="loader-circle" spin />{stage.text}</p>
    {!state?.in_flight && signInRunning(state) && <span><Button onClick={onCancel}>Cancel sign-in</Button></span>}
  </div>;
  if (stage.kind === "signed_in") return <div className="success" role="status"><Icon name="circle-check" size={24} /><p>{stage.line}</p></div>;
  if (stage.kind === "failed") {
    const retry = stage.hint === "expired" ? "Get a new code" : stage.hint === "check" ? "Check again" : "Try again";
    return <div className="stack-small">
      <Notice tone="danger" alert>{stage.text}</Notice>
      <div className="row">
        {stage.hint === "missing" ? onUseKey && <Button variant="primary" onClick={onUseKey}>Use an API key instead</Button>
          : <Button variant="primary" onClick={() => stage.hint === "check" ? onCheck() : onStart(method)}>{retry}</Button>}
      </div>
    </div>;
  }
  if (stage.kind === "start") {
    if (method === "codex_existing") return <div className="stack-small">
      <p>CrazyAgent copies this sign-in into its own private folder for this account. Codex itself is not changed.</p>
      <span><Button variant="primary" icon="log-in" onClick={() => onStart("codex_existing")}>Use this sign-in</Button></span>
    </div>;
    if (provider === "claude") return <span><Button variant="primary" icon="log-in" onClick={() => onStart("claude_official")}>Open Claude sign-in</Button></span>;
    return <div className="stack-small">
      <span><Button variant="primary" icon="key-round" onClick={() => onStart("codex_device_code")}>Get sign-in code</Button></span>
      <span><Button variant="quiet" onClick={() => onStart("codex_browser")}>Sign in in this browser instead</Button></span>
    </div>;
  }
  return <div className="stack">
    {stage.challenge && <ChallengeView challenge={stage.challenge} />}
    <p className="live" role="status"><Icon name="loader-circle" spin />
      {stage.codeSent ? "Code sent. Waiting for Claude to finish signing in..." : "Waiting for you to finish signing in..."}</p>
    {stage.claudeInput && <ClaudeCode key={state?.operation?.operation_id} onSend={onClaudeCode} />}
    <div className="row"><Button onClick={onCancel}>Cancel sign-in</Button><Button variant="quiet" onClick={onCheck}>Check now</Button></div>
  </div>;
}
