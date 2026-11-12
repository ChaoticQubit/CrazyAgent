import type { Trust, TrustView } from "../../api-projects";

/** What raising trust unlocks, shown before the owner confirms it, the same in the TUI. */
export const RAISE_WARNING = "Reviewed lets this repository's settings, hooks, MCP servers and instruction files reach agents, checks, engine git and the IDE, and lets you choose Own branch or Full access. Raise it only after you have read the code.";

/** What lowering trust does to saved access and running agents, the same in the TUI. */
export const LOWERING_LINE = "Untrusted lowers a saved Own branch or Full access to Sandboxed and stops agents running above it.";

/** What Repository trust says after a save refused because the level changed since the page
 * read it, once it shows the saved level again. */
export const TRUST_STALE = "Repository trust changed since you opened it. The page now shows the saved level; make your change again.";

/** Whether moving from `from` to `to` raises trust, the one change that needs the owner's
 * confirm. Inputs: the saved and the chosen level. Output: true for a raise. */
export function raises(from: Trust, to: Trust): boolean {
  return from === "untrusted" && to === "reviewed";
}

/** Why the project has its level, in the owner's words, the same in the TUI. Input: the
 * engine's view. Output: the line. */
export function originLine(view: TrustView): string {
  if (view.origin === "cloned") return "Set when CrazyAgent cloned this repository from a URL.";
  if (view.origin === "owner") return "You chose this level.";
  return "The default for a project you made or added.";
}

/** Names a trust level as the engine labels it. Inputs: the view and the level. Output: the label. */
export function trustLabel(view: TrustView, trust: Trust): string {
  return view.levels.find((level) => level.trust === trust)?.label ?? trust;
}

/** The trust level choice as radio cards with each level's meaning, the reason the project has
 * its level and what lowering does. Inputs: the radio group name, the view, the chosen level and
 * the change handler. Output: the fieldset. */
export function TrustLevels({ name, view, value, onChange }: {
  name: string; view: TrustView; value: Trust; onChange: (trust: Trust) => void;
}) {
  return <fieldset className="pj-field">
    <legend>Repository trust</legend>
    <div className="pj-radio-cards" role="radiogroup" aria-label="Repository trust">
      {view.levels.map((level) => <label className={`pj-radio-card ${value === level.trust ? "selected" : ""}`} key={level.trust}>
        <input type="radio" name={name} value={level.trust} checked={value === level.trust} onChange={() => onChange(level.trust)} />
        <span className="pj-radio-body">
          <strong>{level.label}</strong>
          <span className="pj-secondary">{level.description}</span>
        </span>
      </label>)}
    </div>
    <p className="pj-help">{originLine(view)}</p>
    {value === "untrusted" && view.trust === "reviewed" && <p className="pj-note warn" role="alert">{LOWERING_LINE}</p>}
  </fieldset>;
}

/** Every rule Untrusted locks with why, and every gap no level closes with its effect, as the
 * engine sends them. Input: the view. Output: the two lists. */
export function TrustRules({ view }: { view: TrustView }) {
  return <>
    <section className="pj-stack" aria-label="Locked while Untrusted">
      <h3>Locked while Untrusted</h3>
      <ul className="pj-rules">{view.rules.map((rule) => <li key={rule.id} data-rule={rule.id}>
        <span>{rule.text}</span>
        <span className="pj-secondary">Why: {rule.reason}</span>
      </li>)}</ul>
    </section>
    <section className="pj-stack" aria-label="Not covered by any level">
      <h3>Not covered by any level</h3>
      <ul className="pj-rules">{view.gaps.map((gap) => <li key={gap.id} data-gap={gap.id}>
        <span><strong>{gap.id}</strong> {gap.text}</span>
        <span className="pj-secondary">{gap.effect}</span>
      </li>)}</ul>
    </section>
  </>;
}
