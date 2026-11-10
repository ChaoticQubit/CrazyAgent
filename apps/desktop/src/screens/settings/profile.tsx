import { useEffect, useState, type FormEvent } from "react";
import { applyTheme, readTerminalInput, readTheme, saveTerminalInput, useAppSession, type ThemeChoice } from "../../state";
import { INPUT_CHOICES, type InputChoice } from "../team/terminal-io";
import { Button, LoadError, NotReady, Notice, Segmented, SettingLine, SettingSection, Skeleton } from "../../ui";
import { ProfileFields, draftFromProfile, emptyProfileDraft, submitProfile, type ProfileDraft, type ProfileErrors } from "../first-run";

function Appearance() {
  const [theme, setTheme] = useState<ThemeChoice>(readTheme);
  const [input, setInput] = useState<InputChoice>(readTerminalInput);
  return <SettingSection title="Appearance">
    <SettingLine label="Theme" help="System follows your Mac. This choice is kept in this app only.">
      <Segmented label="Theme" value={theme} onValue={(value) => { setTheme(value); applyTheme(value); }} options={[
        { value: "system", label: "System", icon: "monitor" }, { value: "light", label: "Light", icon: "sun" }, { value: "dark", label: "Dark", icon: "moon" },
      ]} />
    </SettingLine>
    <SettingLine label="Terminal input" help="Where you type to the master and every agent, on every screen: straight into the terminal, in a message box under it that sends your text as one message, or both. Kept in this app only.">
      <Segmented label="Terminal input" value={input} onValue={(value) => { setInput(value); saveTerminalInput(value); }} options={INPUT_CHOICES} />
    </SettingLine>
  </SettingSection>;
}

/** Settings, Profile: the Welcome fields saved with Save changes (only after a change), plus
 * Appearance. Reads the profile from the session. */
export function ProfileSettings() {
  const session = useAppSession();
  const saved = session.profile.kind === "ok" ? session.profile.value : null;
  const [draft, setDraft] = useState<ProfileDraft>(() => saved ? draftFromProfile(saved) : emptyProfileDraft());
  const [errors, setErrors] = useState<ProfileErrors>({});
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState("");
  useEffect(() => { if (saved) setDraft(draftFromProfile(saved)); }, [saved]);
  const changed = JSON.stringify(draft) !== JSON.stringify(saved ? draftFromProfile(saved) : emptyProfileDraft());
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (saving || !changed) return;
    setSaving(true);
    setFailure("");
    const result = await submitProfile(draft);
    setSaving(false);
    if ("errors" in result) { setErrors(result.errors); return; }
    setErrors({});
    if ("failure" in result) { setFailure(result.failure); return; }
    await session.refreshProfile();
    session.showToast("Profile saved.");
  }
  return <div className="stack">
    <div className="settings-top"><div><h2>Profile</h2><p>Agents read this before they work for you.</p></div></div>
    {session.profile.kind === "loading" ? <Skeleton shape="line" count={6} />
      : session.profile.kind === "not_ready" ? <NotReady what="Your profile" compact />
      : session.profile.kind === "error" ? <LoadError message="Couldn't load your profile." onRetry={() => void session.refreshProfile()} />
      : <SettingSection title="Profile">
        <form aria-label="Profile" noValidate onSubmit={(event) => void submit(event)}>
          <ProfileFields draft={draft} onDraft={setDraft} errors={errors} rows />
          <div className="set-body">
            {failure && <Notice tone="danger" alert>{failure}</Notice>}
            <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
          </div>
        </form>
      </SettingSection>}
    <Appearance />
  </div>;
}
