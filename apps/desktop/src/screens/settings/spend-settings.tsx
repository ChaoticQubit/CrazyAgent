import { useRef, useState } from "react";
import { SPEND_DEFAULTS, setSpendOne, usageRefusal, type AllLimited, type LimitedPool, type ResetsSource, type SpendChoice, type SpendConfirm, type SpendSettings, type SpendView, type UsageReport } from "../../api-usage";
import { useHeld, type Loaded } from "../../state";
import { LoadError, Notice, Segmented, SettingLine, SettingSection } from "../../ui";
import { andList } from "./account-drawer";
import { SpendReset } from "./spend-reset";

/** The Spend confirmation options, the default first. */
export const CONFIRM_OPTIONS: { value: SpendConfirm; label: string }[] = [
  { value: "confirm_each", label: "Confirm each" },
  { value: "one_click", label: "One click" },
  { value: "off", label: "Off" },
];

/** The options for when every account in a pool is limited, the default first. */
export const ALL_LIMITED_OPTIONS: { value: AllLimited; label: string }[] = [
  { value: "notice", label: "Notice" },
  { value: "no_hint", label: "No hint" },
  { value: "auto_spend", label: "Spend on its own" },
];

/** The Claude free resets sources, the default first. */
export const RESETS_SOURCE_OPTIONS: { value: ResetsSource; label: string }[] = [
  { value: "off", label: "Don't read" },
  { value: "through_claude_code", label: "Through Claude Code" },
  { value: "as_claude_code", label: "As Claude Code" },
];

const RESETS_SOURCE_HELP: Record<ResetsSource, string> = {
  off: "CrazyAgent never asks for Claude free resets, and Claude accounts say off in Settings. Don't read is the default.",
  through_claude_code: "Until Claude Code can report free resets itself, CrazyAgent asks Anthropic as Claude Code, the same as As Claude Code. Anthropic may treat this as against its terms, and the risk falls on your Claude accounts.",
  as_claude_code: "CrazyAgent asks Anthropic for free resets as Claude Code, the way Claude Code itself asks. Anthropic may treat this as against its terms, and the risk falls on your Claude accounts.",
};

/** The help line under Claude free resets. Input: the source. Output: the sentence. */
export function resetsSourceHelp(value: ResetsSource): string {
  return RESETS_SOURCE_HELP[value];
}

const CONFIRM_HELP: Record<SpendConfirm, string> = {
  confirm_each: "Each free reset asks you before CrazyAgent uses it.",
  one_click: "A free reset is used as soon as you click.",
  off: "CrazyAgent never uses a free reset. Use them in the provider's own app.",
};

const ALL_LIMITED_HELP: Record<AllLimited, string> = {
  notice: "When every account in a pool is at its limit, Pools names the accounts that hold a free reset.",
  no_hint: "Nothing is shown when every account in a pool is at its limit.",
  auto_spend: "When every account in a pool is at its limit, CrazyAgent uses one free reset on its own: at most once an hour per pool, once per limit window per account, and only on a fresh count.",
};

/** The help line under Spend confirmation. Input: the option. Output: the sentence. */
export function confirmHelp(value: SpendConfirm): string {
  return CONFIRM_HELP[value];
}

/** The help line under the all-limited setting; with spending off, auto-spend says it does
 * nothing. Input: the settings. Output: the sentence. */
export function allLimitedHelp(settings: SpendSettings): string {
  if (settings.confirm === "off" && settings.all_limited === "auto_spend") return "Spending is off, so CrazyAgent never uses a free reset on its own.";
  return ALL_LIMITED_HELP[settings.all_limited];
}

/** The three free resets settings as drawn, each option a radio with its help line and a
 * changed mark off the default; a pick names only its own setting. Inputs: the settings, the
 * refusal to show and the pick handler. Output: the section. */
export function SpendSettingsView({ settings, refused, onChoice }: {
  settings: SpendSettings; refused: string; onChoice: (choice: SpendChoice) => void;
}) {
  const confirmChanged = settings.confirm !== SPEND_DEFAULTS.confirm;
  const limitedChanged = settings.all_limited !== SPEND_DEFAULTS.all_limited;
  const source = settings.resets_source ?? SPEND_DEFAULTS.resets_source;
  const sourceChanged = source !== SPEND_DEFAULTS.resets_source;
  return <SettingSection title="Free resets" about={SPEND_ABOUT}>
    <SettingLine label="Spend confirmation" help={confirmHelp(settings.confirm)} changed={confirmChanged}>
      <Segmented label="Spend confirmation" value={settings.confirm} options={CONFIRM_OPTIONS} onValue={(value) => onChoice({ field: "confirm", value })} />
    </SettingLine>
    <SettingLine label="When every pool account is limited" help={allLimitedHelp(settings)} changed={limitedChanged}>
      <Segmented label="When every pool account is limited" value={settings.all_limited} options={ALL_LIMITED_OPTIONS} onValue={(value) => onChoice({ field: "all_limited", value })} />
    </SettingLine>
    <SettingLine label="Claude free resets" help={resetsSourceHelp(source)} changed={sourceChanged}>
      <Segmented label="Claude free resets" value={source} options={RESETS_SOURCE_OPTIONS} onValue={(value) => onChoice({ field: "resets_source", value })} />
    </SettingLine>
    {refused && <div className="set-body"><Notice tone="danger" alert>{refused}</Notice></div>}
  </SettingSection>;
}

const SPEND_ABOUT = "Some accounts hold free resets that start their limits over. These choose how CrazyAgent uses them.";

/** Runs saves one at a time in the order asked: a save asked while another is on its way waits
 * for it, so the engine keeps the picks of one setting in the order they were made. Input: the
 * save. Output: the call that asks for a save and answers with its result. */
export function inOrder<T, R>(save: (value: T) => Promise<R>): (value: T) => Promise<R> {
  let last: Promise<unknown> = Promise.resolve();
  return (value) => {
    const sent = last.then(() => save(value));
    last = sent.then(() => undefined, () => undefined);
    return sent;
  };
}

const saveSpend = inOrder(setSpendOne);

type SpendField = SpendChoice["field"];

/** The three free resets settings on Pools: until the usage read holds the stored settings, a
 * loading line, its failure with Try again or a not ready line and no control, so nothing shows
 * a default as stored or saves one; then each control shows its pick at once and stores only its
 * own setting, one save at a time in the order picked. Only a setting's latest pick shows its
 * refusal, until that setting's next answer, and each pick shows until a read started after its
 * answer lands. Inputs: the usage read, the reread and the retry. Output: the section. */
export function SpendSettingsSection({ usage, onDone, onRetry }: { usage: Loaded<UsageReport>; onDone: () => void; onRetry: () => void }) {
  const [refused, setRefused] = useState<Partial<Record<SpendField, string>>>({});
  const latest = useRef<Partial<Record<SpendField, number>>>({});
  const picks = useRef(0);
  const read = usage.kind === "ok" ? usage.value : undefined;
  const stored = read?.spend?.settings;
  const [confirm, holdConfirm] = useHeld<SpendConfirm | undefined>(stored?.confirm, Object.is, read);
  const [allLimited, holdAllLimited] = useHeld<AllLimited | undefined>(stored?.all_limited, Object.is, read);
  const [source, holdSource] = useHeld<ResetsSource | undefined>(stored && (stored.resets_source ?? SPEND_DEFAULTS.resets_source), Object.is, read);
  const choose = (choice: SpendChoice) => {
    const settle = choice.field === "confirm" ? holdConfirm(choice.value)
      : choice.field === "all_limited" ? holdAllLimited(choice.value) : holdSource(choice.value);
    picks.current += 1;
    const mine = picks.current;
    latest.current[choice.field] = mine;
    void saveSpend(choice).then((result) => {
      if (latest.current[choice.field] !== mine) return;
      const note = result.kind === "ok" ? "" : usageRefusal(result.kind === "error" ? result : { code: "usage_unavailable" });
      setRefused((now) => ({ ...now, [choice.field]: note }));
      settle(note);
      onDone();
    });
  };
  if (confirm === undefined || allLimited === undefined || source === undefined) {
    return <SettingSection title="Free resets" about={SPEND_ABOUT}><div className="set-body">
      {usage.kind === "loading" && <p className="secondary" role="status">Loading your free resets settings...</p>}
      {usage.kind === "error" && <LoadError message={usage.message} onRetry={onRetry} />}
      {(usage.kind === "not_ready" || usage.kind === "ok") && <p className="secondary" role="status">The free resets settings aren't in this build yet.</p>}
    </div></SettingSection>;
  }
  const notes = [...new Set([refused.confirm, refused.all_limited, refused.resets_source].filter((note): note is string => Boolean(note)))];
  return <SpendSettingsView settings={{ confirm, all_limited: allLimited, resets_source: source }} refused={notes.join(" ")} onChoice={choose} />;
}

/** The notice line for a pool whose every account is at its limit. Inputs: the pool and the
 * account names. Output: the sentence, naming the accounts that hold a free reset. */
export function limitedPoolLine(pool: LimitedPool, names: ReadonlyMap<string, string>): string {
  const where = pool.name ? `Every account in ${pool.name} is at its limit.` : "Every account in a pool is at its limit.";
  if (pool.with_resets.length === 0) return `${where} None of them has a free reset.`;
  return `${where} Free resets: ${andList(pool.with_resets.map((account) => names.get(account) ?? "an account"))}.`;
}

/** The notice for each pool whose every account is at its limit, with a "Use a free reset"
 * control per account that holds one. The engine fills the pools only while the notice is the
 * setting. Inputs: the spend view, the account names, whether the app can spend for an account
 * and the reread. Output: the notices, or nothing. */
export function LimitedPoolsNotice({ spend, names, inApp, onDone }: {
  spend: SpendView | undefined; names: ReadonlyMap<string, string>; inApp: (account: string) => boolean; onDone: () => void;
}) {
  if (!spend || spend.limited_pools.length === 0) return null;
  return <div className="stack-small">
    {spend.limited_pools.map((pool) => <Notice key={pool.pool} tone="info">
      <span className="grow">{limitedPoolLine(pool, names)}</span>
      {pool.with_resets.map((account) => <SpendReset key={account} account={account} name={names.get(account) ?? "this account"}
        inApp={inApp(account)} mayBreak resetsLeft={null} spend={spend} onDone={onDone} />)}
    </Notice>)}
  </div>;
}
