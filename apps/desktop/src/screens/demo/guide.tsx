import { useState } from "react";
import { Icon } from "../../ui";
import { guideSteps, type Guide } from "./model";
import { useDemo } from "./offer";
import { DEMO_COPY_GOAL, DEMO_GOAL_LABEL, DEMO_GUIDE_TITLE, type DemoStepId } from "./words";
import "./demo.css";

const ACTIONS: Readonly<Record<DemoStepId, string>> = {
  goal: "Open Conversation",
  agents: "Open Agents",
  question: "Open Questions",
  changes: "Open Changes",
  delete: "Open Remove",
};

/** The guide panel on the demo project's page: every step with its tick and two sentences,
 * the goal to send with a copy action under the first, and the current step's link to the tab
 * where it happens. Inputs: the guide, the engine's goal words, whether the goal was just
 * copied, and the copy handler. Output: the panel. */
export function DemoGuideView({ guide, goal, copied, onCopy }: { guide: Guide; goal: string; copied: boolean; onCopy: () => void }) {
  return <section className="demo-guide" aria-label={DEMO_GUIDE_TITLE}>
    <div className="demo-guide-head">
      <Icon name="lightbulb" size={16} /><h2>{DEMO_GUIDE_TITLE}</h2>
      <span className="secondary">Step {guide.current + 1} of {guide.steps.length}</span>
    </div>
    <ol>
      {guide.steps.map((step, index) => <li key={step.id} data-done={step.done} aria-current={index === guide.current ? "step" : undefined}>
        <span className="row"><Icon name={step.done ? "circle-check" : "circle-dashed"} size={14} /><strong>{step.title}</strong></span>
        <p className="secondary">{step.body}</p>
        {step.id === "goal" && <div className="demo-goal">
          <span className="label">{DEMO_GOAL_LABEL}</span>
          <p>{goal}</p>
          <button type="button" className="btn quiet small" onClick={onCopy}><Icon name={copied ? "check" : "copy"} size={14} />{copied ? "Copied" : DEMO_COPY_GOAL}</button>
        </div>}
        {index === guide.current && <span><a className={`btn small${step.id === "delete" ? "" : " primary"}`} href={step.href}>{ACTIONS[step.id]}</a></span>}
      </li>)}
    </ol>
  </section>;
}

/** The guide, reading the demo itself so its mount on the project page is one line. It shows
 * only on the demo project's page and follows the engine while a step is open. Input: the
 * page's project id. Output: the panel, or nothing on any other project. */
export function DemoGuide({ projectId }: { projectId: string }) {
  const [view] = useDemo();
  const [copied, setCopied] = useState(false);
  const guide = view.kind === "ok" ? guideSteps(view.value, projectId) : null;
  if (!guide || view.kind !== "ok") return null;
  const goal = view.value.goal;
  const copy = () => void navigator.clipboard?.writeText(goal).then(() => {
    setCopied(true);
    window.setTimeout(() => setCopied(false), 2000);
  }, () => setCopied(false));
  return <DemoGuideView guide={guide} goal={goal} copied={copied} onCopy={copy} />;
}
