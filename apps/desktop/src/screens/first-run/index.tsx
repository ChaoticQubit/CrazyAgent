import { useState, type FormEvent } from "react";
import { saveProfile, type OwnerProfile } from "../../api";
import { BrandMark, Button, ChipInput, Notice, Segmented, TextArea, TextField, navigate, type Route } from "../../ui";
import { HomeFolderPicker } from "../intake/home-folder";
import { ToolsSetup } from "../intake/setup-step";
import "../intake/intake.css";

export interface ProfileDraft {
  name: string;
  role: string;
  bio: string;
  stack: string[];
  detail: string;
  initiative: string;
}

export type ProfileErrors = Partial<Record<"name" | "role" | "bio" | "stack" | "style", string>>;

const NAME_MAX = 80;
const ROLE_MAX = 80;
const BIO_MAX = 500;
const STACK_MAX = 12;
const STACK_ITEM_MAX = 40;
const ONE_LINE = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]/u;
const TEXT_CONTROLS = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u;

export const STACK_SUGGESTIONS = [
  "TypeScript", "JavaScript", "Python", "Rust", "Go", "Swift", "Kotlin", "Java", "C#", "Ruby", "PHP",
  "React", "Next.js", "Vue", "Svelte", "Tauri", "Node.js", "Django", "FastAPI", "Rails",
  "PostgreSQL", "SQLite", "MySQL", "Redis", "Tailwind CSS", "Docker", "AWS",
];

/** An empty first-run draft with the default working style: short replies, ask first.
 * Output: the draft. */
export function emptyProfileDraft(): ProfileDraft {
  return { name: "", role: "", bio: "", stack: [], detail: "terse", initiative: "ask_first" };
}

/** Loads a saved engine profile into an editable draft. Input: the profile. Output: the draft. */
export function draftFromProfile(profile: OwnerProfile): ProfileDraft {
  return { name: profile.name, role: profile.role, bio: profile.bio, stack: [...profile.preferred_stack],
    detail: profile.working_style.detail, initiative: profile.working_style.initiative };
}

const length = (value: string) => [...value].length;

/** Checks a profile draft against the design's bounds (stricter than the engine's) and
 * builds the engine profile. Input: the draft. Output: the profile or per-field errors. */
export function validateProfile(draft: ProfileDraft): { profile: OwnerProfile } | { errors: ProfileErrors } {
  const errors: ProfileErrors = {};
  const name = draft.name.trim();
  const role = draft.role.trim();
  const bio = draft.bio.trim();
  if (!name) errors.name = "Enter your name.";
  else if (ONE_LINE.test(name)) errors.name = "Keep your name on one line.";
  else if (length(name) > NAME_MAX) errors.name = `Keep your name under ${NAME_MAX + 1} characters.`;
  if (ONE_LINE.test(role)) errors.role = "Keep your role on one line.";
  else if (length(role) > ROLE_MAX) errors.role = `Keep your role under ${ROLE_MAX + 1} characters.`;
  if (TEXT_CONTROLS.test(bio)) errors.bio = "Remove special characters from your bio.";
  else if (length(bio) > BIO_MAX) errors.bio = `Keep your bio under ${BIO_MAX + 1} characters.`;
  if (draft.stack.length > STACK_MAX) errors.stack = `List up to ${STACK_MAX} technologies.`;
  else if (draft.stack.some((item) => length(item) > STACK_ITEM_MAX || ONE_LINE.test(item) || !item.trim())) errors.stack = `Keep each technology under ${STACK_ITEM_MAX + 1} characters.`;
  const detail = draft.detail === "terse" || draft.detail === "detailed" ? draft.detail : undefined;
  const initiative = draft.initiative === "ask_first" || draft.initiative === "act_first" ? draft.initiative : undefined;
  if (!detail || !initiative) errors.style = "Choose how agents reply and act.";
  if (Object.keys(errors).length > 0 || !detail || !initiative) return { errors };
  return { profile: { name, role, bio, preferred_stack: draft.stack.map((item) => item.trim()), working_style: { detail, initiative } } };
}

/** The profile fields shared by Welcome and Settings, Profile. Inputs: the draft, its change
 * handler and field errors. Output: the fields. */
export function ProfileFields({ draft, onDraft, errors }: { draft: ProfileDraft; onDraft: (draft: ProfileDraft) => void; errors: ProfileErrors }) {
  const set = (patch: Partial<ProfileDraft>) => onDraft({ ...draft, ...patch });
  return <>
    <TextField id="profile-name" label="Name" value={draft.name} onValue={(name) => set({ name })} maxLength={NAME_MAX} required
      autoComplete="name" error={errors.name} />
    <TextField id="profile-role" label="Role" value={draft.role} onValue={(role) => set({ role })} maxLength={ROLE_MAX}
      helper="For example: Founder, Full-stack engineer, Designer" error={errors.role} />
    <TextArea id="profile-bio" label="Short bio" value={draft.bio} onValue={(bio) => set({ bio })} rows={3} maxLength={BIO_MAX} counterAt={450}
      helper="What you work on and what you care about." error={errors.bio} />
    <ChipInput id="profile-stack" label="Preferred stack" values={draft.stack} onValues={(stack) => set({ stack })}
      suggestions={STACK_SUGGESTIONS} max={STACK_MAX} maxLength={STACK_ITEM_MAX} helper={errors.stack ?? "Agents use these unless a project says otherwise."} />
    <div className="two">
      <div className="field"><span className="field-label" id="profile-replies">Replies</span>
        <Segmented label="Replies" value={draft.detail} onValue={(detail) => set({ detail })}
          options={[{ value: "terse", label: "Short" }, { value: "detailed", label: "Detailed" }]} /></div>
      <div className="field"><span className="field-label" id="profile-acting">Before acting</span>
        <Segmented label="Before acting" value={draft.initiative} onValue={(initiative) => set({ initiative })}
          options={[{ value: "ask_first", label: "Ask first" }, { value: "act_first", label: "Act first" }]} /></div>
    </div>
    {errors.style && <p className="field-error" role="alert">{errors.style}</p>}
  </>;
}

/** Saves a validated profile and maps the engine reply to the form's status line.
 * Input: the draft. Output: errors, a save failure message, or success. */
export async function submitProfile(draft: ProfileDraft): Promise<{ errors: ProfileErrors } | { failure: string; notReady: boolean } | { saved: OwnerProfile }> {
  const checked = validateProfile(draft);
  if ("errors" in checked) return checked;
  const result = await saveProfile(checked.profile);
  if (result.kind === "ok") return { saved: checked.profile };
  if (result.kind === "not_ready") return { failure: "Saving your profile isn't ready in this build yet.", notReady: true };
  return { failure: result.code === "uncertain" ? result.message : `Couldn't save your profile. ${result.message}`, notReady: false };
}

export type FirstRunStep = "profile" | "home" | "tools" | "accounts";

export const FIRST_RUN_STEPS: { id: FirstRunStep; label: string; title: string; intro: string }[] = [
  { id: "profile", label: "Profile", title: "Tell CrazyAgent about you",
    intro: "Agents read this before they work for you. You can change it later in Settings, Profile." },
  { id: "home", label: "Projects folder", title: "Choose your projects folder",
    intro: "Every project lives in its own folder here. CrazyAgent keeps its own data in a private folder, not in this one." },
  { id: "tools", label: "Tools", title: "Set up tools",
    intro: "CrazyAgent installs tmux, uv and graphify when they're missing, at pinned versions it checks before running, plus the default pack of skills and plugins." },
  { id: "accounts", label: "Accounts", title: "Add an account",
    intro: "Agents need a model account to work. Sign in with ChatGPT or Claude, or add an API key or a local model." },
];

/** The first-run flow with no sidebar, in the order profile, projects folder, tools, accounts:
 * the brand, the step list and a short intro on the left, the step on the right. Finishing goes
 * to Home or to Add account. Inputs: the saved handler, an optional skip handler used when the
 * engine has no profile support yet, and the step to start on. */
export function Welcome({ onSaved, onSkip, initialStep = "profile" }: {
  onSaved: (profile: OwnerProfile, next?: Route) => void;
  onSkip?: (next?: Route) => void;
  initialStep?: FirstRunStep;
}) {
  const [step, setStep] = useState<FirstRunStep>(initialStep);
  const [saved, setSaved] = useState<OwnerProfile | null>(null);
  const [draft, setDraft] = useState(emptyProfileDraft);
  const [errors, setErrors] = useState<ProfileErrors>({});
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<{ message: string; notReady: boolean }>();
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (saving) return;
    setSaving(true);
    setFailure(undefined);
    const result = await submitProfile(draft);
    setSaving(false);
    if ("errors" in result) {
      setErrors(result.errors);
      document.getElementById(`profile-${Object.keys(result.errors)[0]}`)?.focus();
      return;
    }
    setErrors({});
    if ("failure" in result) setFailure({ message: result.failure, notReady: result.notReady });
    else { setSaved(result.saved); setStep("home"); }
  }
  const finish = (next: Route) => {
    if (saved) onSaved(saved, next);
    else onSkip?.(next);
  };
  const current = FIRST_RUN_STEPS.find((item) => item.id === step) ?? FIRST_RUN_STEPS[0];
  return <main id="main" className="welcome">
    <div className="intro">
      <BrandMark large />
      <ol className="in-steps" aria-label="Setup steps">{FIRST_RUN_STEPS.map((item) =>
        <li key={item.id} aria-current={item.id === step ? "step" : undefined}>{item.label}</li>)}</ol>
      <h1>{current.title}</h1>
      <p>{current.intro}</p>
      <span><Button variant="quiet" icon="refresh-cw" onClick={() => navigate({ name: "settings", tab: "updates" })}>Check for updates</Button></span>
    </div>
    {step === "profile" && <form className="form" aria-label="Your profile" noValidate onSubmit={(event) => void submit(event)}>
      <ProfileFields draft={draft} onDraft={setDraft} errors={errors} />
      {failure && <Notice tone={failure.notReady ? "info" : "danger"} alert>{failure.message}</Notice>}
      <div className="row">
        <Button type="submit" variant="primary" size="large" pending={saving} pendingLabel="Saving...">Save and continue</Button>
        {failure?.notReady && onSkip && <Button variant="quiet" onClick={() => setStep("home")}>Continue without saving</Button>}
      </div>
    </form>}
    {step === "home" && <section className="form" aria-label="Projects folder">
      <HomeFolderPicker onSaved={() => setStep("tools")} onContinue={() => setStep("tools")} />
    </section>}
    {step === "tools" && <section className="form" aria-label="Tools">
      <ToolsSetup onContinue={() => setStep("accounts")} />
    </section>}
    {step === "accounts" && <section className="form" aria-label="Accounts">
      <div className="row">
        <Button variant="primary" size="large" icon="key-round" onClick={() => finish({ name: "settings", tab: "accounts", action: "add" })}>Add account</Button>
        <Button variant="quiet" onClick={() => finish({ name: "home" })}>Skip for now</Button>
      </div>
    </section>}
  </main>;
}
