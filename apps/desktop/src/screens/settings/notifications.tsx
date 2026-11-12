import { useEffect, useState, type FormEvent } from "react";
import {
  NOTICE_DEFAULTS, NOTICE_KINDS, RETENTION_DAYS_MAX, RETENTION_DAYS_MIN, deliveryChoices, loadNoticeSettings, retentionDays, sendNotice,
  type Banners, type Delivery, type NoticeClass, type NoticeKind, type NoticeSettings,
} from "../../api-notifications";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, SettingLine, SettingSection, Skeleton } from "../../ui";
import "../notifications/notifications.css";
import "../project/projects.css";

/** The words each delivery choice shows, the same in the TUI. */
export const DELIVERY_LABELS: Record<Delivery, string> = { on: "On", bell_only: "Bell only", off: "Off" };

/** The macOS notifications choices in the order Settings lists them, with their words. */
export const BANNER_CHOICES: { choice: Banners; label: string }[] = [
  { choice: "off", label: "Off" }, { choice: "action_only", label: "Action items only" }, { choice: "all", label: "All" },
];

const SECTIONS: { kind: NoticeClass; title: string; about: string }[] = [
  { kind: "action", title: "Action items", about: "Things waiting on you. They stay pending until they are dealt with, so they can be quiet but never off. On also sends a macOS notification; Bell only keeps them to the bell." },
  { kind: "info", title: "Info items", about: "Things that happened. On also sends a macOS notification, Bell only keeps them to the bell, and Off keeps them out of the store." },
];

const TOP = <div className="settings-top"><div>
  <h2>Notifications</h2>
  <p>What reaches you, and how long finished items stay.</p>
</div></div>;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** One kind's stored delivery. Inputs: the stored settings and the kind. Output: its
 * delivery; the engine lists every kind, so a missing one is a broken invariant. */
function storedDelivery(stored: NoticeSettings, kind: NoticeKind): Delivery {
  const item = stored.kinds.find((entry) => entry.kind === kind);
  assert(item !== undefined, `the engine settings leave out ${kind}`);
  return item.delivery;
}

/** The settings a save sends. Inputs: the stored settings, the picked deliveries, the
 * picked macOS choice and the typed days, null for each left alone. Output: every kind in
 * engine order with the picks applied, or null when the typed days are not 1 to 365. */
export function savedSettings(stored: NoticeSettings, kinds: ReadonlyMap<NoticeKind, Delivery>, banners: Banners | null, typed: string | null): NoticeSettings | null {
  const days = typed === null ? stored.retention_days : retentionDays(typed);
  if (days === null) return null;
  const picked = NOTICE_KINDS.map((entry) => {
    const delivery = kinds.get(entry.kind) ?? storedDelivery(stored, entry.kind);
    assert(deliveryChoices(entry.kind).includes(delivery), `${entry.kind} cannot be ${delivery}`);
    return { kind: entry.kind, delivery };
  });
  assert(picked.length === NOTICE_KINDS.length, "a save lists every kind once");
  return { kinds: picked, retention_days: days, banners: banners ?? stored.banners };
}

/** Whether a save would change what the engine holds. Inputs: the stored and the sent
 * settings. Output: true when any kind, the days or the macOS choice differ. */
function differs(stored: NoticeSettings, sent: NoticeSettings): boolean {
  if (sent.retention_days !== stored.retention_days || sent.banners !== stored.banners) return true;
  return sent.kinds.some((item) => item.delivery !== storedDelivery(stored, item.kind));
}

/** A row of radio choices drawn as segments, each a real radio inside its label. Inputs:
 * the group name, the kind or setting it holds, the choices, the picked one, a lock flag
 * and the pick handler. Output: the fieldset. */
function Choices<T extends string>({ name, kind, setting, legend, choices, value, disabled, onPick }: {
  name: string; kind?: NoticeKind; setting?: string; legend: string;
  choices: { choice: T; label: string }[]; value: T; disabled: boolean; onPick: (choice: T) => void;
}) {
  assert(choices.some((option) => option.choice === value), `${name} has no choice ${value}`);
  return <fieldset className="notice-choices" disabled={disabled} data-kind={kind} data-setting={setting}>
    <legend className="visually-hidden">{legend}</legend>
    {choices.map((option) => <label key={option.choice} className={option.choice === value ? "on" : undefined}>
      <input type="radio" name={name} value={option.choice} checked={option.choice === value} onChange={() => onPick(option.choice)} />
      {option.label}
    </label>)}
  </fieldset>;
}

/** One class of kinds as a settings section, each kind with its delivery choices and a
 * changed mark when away from On. Inputs: the class, the shown deliveries, a lock flag and
 * the pick handler. Output: the section. */
function KindSection({ kind, shown, saving, onPick }: {
  kind: NoticeClass; shown: (kind: NoticeKind) => Delivery; saving: boolean; onPick: (kind: NoticeKind, delivery: Delivery) => void;
}) {
  const section = SECTIONS.find((entry) => entry.kind === kind);
  assert(section !== undefined, `no section for ${kind} items`);
  return <SettingSection title={section.title} about={section.about}>
    {NOTICE_KINDS.filter((entry) => entry.class === kind).map((entry) => <SettingLine key={entry.kind} label={entry.label} changed={shown(entry.kind) !== "on"}>
      <Choices name={`delivery-${entry.kind}`} kind={entry.kind} legend={entry.label} value={shown(entry.kind)} disabled={saving}
        choices={deliveryChoices(entry.kind).map((choice) => ({ choice, label: DELIVERY_LABELS[choice] }))} onPick={(choice) => onPick(entry.kind, choice)} />
    </SettingLine>)}
  </SettingSection>;
}

/** Settings, Notifications as a view of its state: the title in every state, then each
 * Action and Info kind with its delivery, the macOS notifications choice and the days Done
 * items stay, and Save changes, enabled only after a valid change. Inputs: the settings
 * read, the picks, the typed days, a saving flag, the last problem and the handlers.
 * Output: the tab. */
export function NotificationSettingsView({ settings, kinds, banners, typedDays, saving, problem, onPickDelivery, onPickBanners, onTypeDays, onSave, onRetry }: {
  settings: Loaded<NoticeSettings>;
  kinds: ReadonlyMap<NoticeKind, Delivery> | null;
  banners: Banners | null;
  typedDays: string | null;
  saving: boolean;
  problem: string;
  onPickDelivery: (kind: NoticeKind, delivery: Delivery) => void;
  onPickBanners: (choice: Banners) => void;
  onTypeDays: (typed: string) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <>{TOP}<Skeleton shape="line" count={6} /></>;
  if (settings.kind === "not_ready") return <>{TOP}<NotReady what="Notifications" /></>;
  if (settings.kind === "error") return <>{TOP}<LoadError message={`Couldn't load the notification settings. ${settings.message}`} onRetry={onRetry} /></>;
  const stored = settings.value;
  const picks = kinds ?? new Map<NoticeKind, Delivery>();
  const sent = savedSettings(stored, picks, banners, typedDays);
  const changed = sent !== null && differs(stored, sent);
  const shown = (kind: NoticeKind) => picks.get(kind) ?? storedDelivery(stored, kind);
  const banner = banners ?? stored.banners;
  const text = typedDays ?? String(stored.retention_days);
  const shownDays = retentionDays(text) ?? stored.retention_days;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <>
    {TOP}
    <form className="pj set-groups" aria-label="Notifications" noValidate onSubmit={submit}>
      <KindSection kind="action" shown={shown} saving={saving} onPick={onPickDelivery} />
      <KindSection kind="info" shown={shown} saving={saving} onPick={onPickDelivery} />
      <SettingSection title="macOS notifications" about="Which items also show as a macOS notification. A click on one opens its page.">
        <SettingLine label="Show" changed={banner !== NOTICE_DEFAULTS.banners}>
          <Choices name="banners" setting="banners" legend="macOS notifications" value={banner} disabled={saving}
            choices={BANNER_CHOICES} onPick={onPickBanners} />
        </SettingLine>
      </SettingSection>
      <SettingSection title="Done items" about="Finished and deleted items leave the Done filter after this many days.">
        <SettingLine label="Keep finished items for" htmlFor="notice-retention-days" changed={shownDays !== NOTICE_DEFAULTS.retention_days}>
          <div className="pj-row">
            <input className="pj-input num" id="notice-retention-days" type="number" min={RETENTION_DAYS_MIN} max={RETENTION_DAYS_MAX} value={text}
              disabled={saving} onChange={(event) => onTypeDays(event.target.value)}
              aria-invalid={sent === null || undefined} aria-describedby={sent === null ? "notice-retention-days-error" : undefined} />
            <span>days</span>
          </div>
        </SettingLine>
        {sent === null && <p className="pj-error" id="notice-retention-days-error" role="alert">{`Enter a whole number of days from ${RETENTION_DAYS_MIN} to ${RETENTION_DAYS_MAX}.`}</p>}
        <div className="set-body">
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
        </div>
      </SettingSection>
    </form>
  </>;
}

/** Settings, Notifications: reads the owner's notification settings and saves the picked
 * deliveries, macOS choice and days in the engine. */
export function NotificationSettings() {
  const session = useAppSession();
  const [settings, reload] = useLoad(loadNoticeSettings);
  const [kinds, setKinds] = useState<ReadonlyMap<NoticeKind, Delivery>>(() => new Map());
  const [banners, setBanners] = useState<Banners | null>(null);
  const [typedDays, setTypedDays] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setKinds(new Map()); setBanners(null); setTypedDays(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok") return;
    const sent = savedSettings(settings.value, kinds, banners, typedDays);
    if (sent === null) return;
    setSaving(true);
    setProblem("");
    const result = await sendNotice({ save_settings: { settings: sent } });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Notification settings saved.");
    reload();
  }
  return <div className="stack">
    <NotificationSettingsView settings={settings} kinds={kinds} banners={banners} typedDays={typedDays} saving={saving} problem={problem}
      onPickDelivery={(kind, delivery) => { setKinds((before) => new Map(before).set(kind, delivery)); setProblem(""); }}
      onPickBanners={(choice) => { setBanners(choice); setProblem(""); }}
      onTypeDays={(text) => { setTypedDays(text); setProblem(""); }} onSave={() => void save()} onRetry={reload} />
  </div>;
}
