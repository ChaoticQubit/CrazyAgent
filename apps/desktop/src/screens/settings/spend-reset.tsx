import { useState } from "react";
import { SPEND_DEFAULTS, spendOutcome, spendReset, usageRefusal, type SpendAttempt, type SpendSettings, type SpendView } from "../../api-usage";
import { Button, ConfirmDialog } from "../../ui";

/** What an account's Spend control shows: nothing, a busy line while a spend waits or is out, or
 * the button, asking first or not, with the latest outcome under it. */
export type SpendFlow = { kind: "hidden" } | { kind: "busy"; line: string } | { kind: "ready"; confirm: boolean; line?: string };

/** Whether the app can spend a free reset for an account of this provider. Only ChatGPT for now:
 * the Claude spend call is still being shaped (DEC-154 addendum S9), and the engine answers it
 * as not available. Input: the account's provider. Output: the answer. */
export function spendsInApp(provider: string | undefined): boolean {
  return provider === "chatgpt";
}

/** Decides an account's Spend control from the owner's Spend confirmation and the account's
 * latest attempt. Off, an account the app cannot spend for, or a count of zero hides it.
 * Inputs: the settings, whether the app can spend for it, the free resets left (null when not
 * reported) and the latest attempt. Output: the flow. */
export function spendFlow({ settings, inApp, resetsLeft, attempt }: {
  settings: SpendSettings; inApp: boolean; resetsLeft: number | null; attempt?: SpendAttempt;
}): SpendFlow {
  if (settings.confirm === "off" || !inApp || resetsLeft === 0) return { kind: "hidden" };
  if (attempt && attempt.state !== "settled") return { kind: "busy", line: spendOutcome(attempt) };
  const confirm = settings.confirm === "confirm_each";
  return attempt ? { kind: "ready", confirm, line: spendOutcome(attempt) } : { kind: "ready", confirm };
}

/** The words of the Spend confirm. Inputs: the account's name and whether the call is a private
 * one that may break. Output: the title, body and confirm label. */
export function confirmCopy(name: string, mayBreak: boolean): { title: string; body: string; confirmLabel: string } {
  const breaks = mayBreak ? "It uses the provider's private call that may break. " : "";
  return {
    title: `Use a free reset on ${name}?`,
    body: `CrazyAgent asks the provider to use one of this account's free resets now, so its limits start over. ${breaks}This can't be undone.`,
    confirmLabel: "Use free reset",
  };
}

/** One account's "Use a free reset" control: the button per the owner's Spend confirmation, the
 * confirm when each spend asks first, and the latest outcome. Inputs: the account id and name,
 * whether the app can spend for it and the call may break, the free resets left, the spend view
 * and what to do once the engine took the spend. Output: the control, or nothing. */
export function SpendReset({ account, name, inApp, mayBreak, resetsLeft, spend, onDone }: {
  account: string; name: string; inApp: boolean; mayBreak: boolean; resetsLeft: number | null; spend: SpendView | undefined; onDone: () => void;
}) {
  const [asking, setAsking] = useState(false);
  const [pending, setPending] = useState(false);
  const [refused, setRefused] = useState("");
  const settings = spend?.settings ?? SPEND_DEFAULTS;
  const attempt = spend?.attempts.find((line) => line.account === account);
  const flow = spendFlow({ settings, inApp, resetsLeft, attempt });
  if (flow.kind === "hidden") return null;
  const go = () => {
    setPending(true);
    void spendReset(account).then((result) => {
      setPending(false);
      setAsking(false);
      setRefused(result.kind === "ok" ? "" : usageRefusal(result.kind === "error" ? result : { code: "usage_unavailable" }));
      if (result.kind === "ok") onDone();
    });
  };
  const copy = confirmCopy(name, mayBreak);
  const line = refused || flow.line;
  return <div className="spend-reset">
    <Button size="small" icon="zap" pending={pending} disabled={flow.kind === "busy"}
      onClick={() => (flow.kind === "ready" && flow.confirm ? setAsking(true) : go())}>Use a free reset</Button>
    {line && <span className="spend-reset-line" role="status">{line}</span>}
    <ConfirmDialog open={asking} title={copy.title} confirmLabel={copy.confirmLabel} pending={pending} onCancel={() => setAsking(false)} onConfirm={go}>
      {copy.body}
    </ConfirmDialog>
  </div>;
}
