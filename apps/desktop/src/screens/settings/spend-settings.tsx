import { useEffect, useState } from "react";
import { SPEND_DEFAULTS, setSpendSettings, usageRefusal, type AllLimited, type LimitedPool, type ResetsSource, type SpendConfirm, type SpendSettings, type SpendView } from "../../api-usage";
import { Notice, Segmented, SettingLine, SettingSection } from "../../ui";
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
 * changed mark off the default. Inputs: the settings, the refusal to show and the change handler. Output:
 * the section. */
export function SpendSettingsView({ settings, refused, onChange }: {
  settings: SpendSettings; refused: string; onChange: (settings: SpendSettings) => void;
}) {
  const confirmChanged = settings.confirm !== SPEND_DEFAULTS.confirm;
  const limitedChanged = settings.all_limited !== SPEND_DEFAULTS.all_limited;
  const source = settings.resets_source ?? SPEND_DEFAULTS.resets_source;
  const sourceChanged = source !== SPEND_DEFAULTS.resets_source;
  return <SettingSection title="Free resets" about="Some accounts hold free resets that start their limits over. These choose how CrazyAgent uses them.">
    <SettingLine label="Spend confirmation" help={confirmHelp(settings.confirm)} changed={confirmChanged}>
      <Segmented label="Spend confirmation" value={settings.confirm} options={CONFIRM_OPTIONS} onValue={(confirm) => onChange({ ...settings, confirm })} />
    </SettingLine>
    <SettingLine label="When every pool account is limited" help={allLimitedHelp(settings)} changed={limitedChanged}>
      <Segmented label="When every pool account is limited" value={settings.all_limited} options={ALL_LIMITED_OPTIONS} onValue={(all_limited) => onChange({ ...settings, all_limited })} />
    </SettingLine>
    <SettingLine label="Claude free resets" help={resetsSourceHelp(source)} changed={sourceChanged}>
      <Segmented label="Claude free resets" value={source} options={RESETS_SOURCE_OPTIONS} onValue={(resets_source) => onChange({ ...settings, resets_source })} />
    </SettingLine>
    {refused && <div className="set-body"><Notice tone="danger" alert>{refused}</Notice></div>}
  </SettingSection>;
}

/** The three free resets settings on Pools: shows the choice at once, stores it, and rereads the usage
 * once the engine kept it. Inputs: the spend view read with the usage and the reread. Output:
 * the section. */
export function SpendSettingsSection({ spend, onDone }: { spend: SpendView | undefined; onDone: () => void }) {
  const [shown, setShown] = useState<SpendSettings>();
  const [refused, setRefused] = useState("");
  const stored = spend?.settings;
  useEffect(() => {
    if (shown && stored && shown.confirm === stored.confirm && shown.all_limited === stored.all_limited
      && (shown.resets_source ?? SPEND_DEFAULTS.resets_source) === (stored.resets_source ?? SPEND_DEFAULTS.resets_source)) setShown(undefined);
  }, [shown, stored]);
  const change = (next: SpendSettings) => {
    setShown(next);
    void setSpendSettings(next).then((result) => {
      setRefused(result.kind === "ok" ? "" : usageRefusal(result.kind === "error" ? result : { code: "usage_unavailable" }));
      if (result.kind === "ok") onDone();
      else setShown(undefined);
    });
  };
  return <SpendSettingsView settings={shown ?? stored ?? SPEND_DEFAULTS} refused={refused} onChange={change} />;
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
