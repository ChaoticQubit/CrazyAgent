import { useEffect } from "react";
import { setLearningMark, type LearningMark, type LearningProgress, type LearningStepId, type LearningTopic } from "../../api-learning";
import type { Loaded, ProjectRow } from "../../state";
import { Button, Icon, Panel, formatRoute, type IconName, type Route } from "../../ui";
import type { SetupItem } from "./index";

/** The heading of each learning topic, the same words as the TUI's Home panel. */
export const TOPIC_HEADINGS: Record<LearningTopic, string> = {
  basics: "Basics",
  core_loop: "Core loop",
  ide_and_terminals: "IDE and terminals",
  process_metrics_usage: "Process, Metrics and Usage",
};

const TOPICS: LearningTopic[] = ["basics", "core_loop", "ide_and_terminals", "process_metrics_usage"];

const ICONS: Record<LearningStepId, IconName> = {
  project: "folder-plus", master: "message-square", agents: "users", question: "inbox", change_set: "git-branch",
  ide: "file-code", terminal: "square-terminal", process: "list-checks", metrics: "trending-up", usage: "zap",
};

/** One row of the panel: a setup item or an engine learning step with its link. */
export interface LearnItem {
  id: SetupItem["id"] | LearningStepId;
  icon: IconName;
  title: string;
  body: string;
  action: string;
  href: string;
  done: boolean;
}

/** One topic of the panel with its heading, or the setup rows alone with no heading. */
export interface LearnGroup {
  heading: string | null;
  items: LearnItem[];
}

/** What Home's side panel shows: its title, its groups, how many counted rows are done,
 * whether the learning steps are in it, and whether it shows at all. */
export interface LearnPanelModel {
  title: string;
  groups: LearnGroup[];
  done: number;
  total: number;
  learning: boolean;
  visible: boolean;
}

/** Says where one learning step's link goes: the first project's page for the project steps,
 * New project while there is none, and the app pages for the rest. Inputs: the step and the
 * first project's id. Output: the link words and route. */
function stepLink(id: LearningStepId, project: string | null): { action: string; href: string } {
  const newProject = { action: "New project", href: formatRoute({ name: "new-project", step: "basics" }) };
  const tab = (action: string, page: "conversation" | "agents" | "questions" | "changes" | "ide") =>
    project === null ? newProject : { action, href: formatRoute({ name: "project", id: project, tab: page }) };
  switch (id) {
    case "project": return newProject;
    case "master": return tab("Open conversation", "conversation");
    case "agents": return tab("Open agents", "agents");
    case "question": return tab("Open questions", "questions");
    case "change_set": return tab("Open changes", "changes");
    case "ide": return tab("Open IDE", "ide");
    case "terminal": return tab("Open agents", "agents");
    case "process": return { action: "Open process", href: formatRoute({ name: "settings", tab: "process" }) };
    case "metrics": return { action: "Open metrics", href: formatRoute({ name: "metrics" }) };
    case "usage": return { action: "Open usage", href: formatRoute({ name: "metrics" }) };
  }
}

/** Works out Home's side panel. Setup keeps its own rule (setupSteps, OP-35, OP-47). The learning
 * steps join it as Learn CrazyAgent, setup under Basics, while the engine reports a step not done
 * and the owner has not skipped the guide. Skipped, every step done, or an engine without the
 * learning read leaves the Get set up panel alone. Nothing shows while any of the five reads is
 * loading, so the panel never flashes. Inputs: the setupSteps result, the learning read and the
 * projects. Output: the panel model. */
export function learnSteps(setup: { items: SetupItem[]; visible: boolean; settled: boolean }, learning: Loaded<LearningProgress>,
  projects: ProjectRow[]): LearnPanelModel {
  const counted = (items: LearnItem[]) => items.filter((item) => item.id !== "harnesses");
  const steps = learning.kind === "ok" && !learning.value.skipped && learning.value.steps.some((step) => step.count === 0)
    ? learning.value.steps : null;
  if (steps === null) {
    const items = counted(setup.items);
    return { title: "Get set up", groups: [{ heading: null, items: setup.items }], done: items.filter((item) => item.done).length,
      total: items.length, learning: false, visible: learning.kind !== "loading" && setup.visible };
  }
  const project = projects[0]?.id ?? null;
  const groups = TOPICS.map((topic) => ({
    heading: TOPIC_HEADINGS[topic],
    items: [
      ...(topic === "basics" ? setup.items : []),
      ...steps.filter((step) => step.topic === topic).map((step): LearnItem => ({
        id: step.id, icon: ICONS[step.id], title: step.title, body: step.body, ...stepLink(step.id, project), done: step.count > 0,
      })),
    ],
  }));
  const items = counted(groups.flatMap((group) => group.items));
  return { title: "Learn CrazyAgent", groups, done: items.filter((item) => item.done).length, total: items.length, learning: true, visible: setup.settled };
}

/** Says which learning marks opening a page sets: the Metrics page shows metrics and usage, a
 * project's Metrics tab metrics, and Settings or a project's Process tab the process. Input:
 * the route. Output: the marks, none for other pages. */
export function marksForRoute(route: Route): LearningMark[] {
  if (route.name === "metrics") return ["metrics", "usage"];
  if (route.name === "settings" && route.tab === "process") return ["process"];
  if (route.name === "project" && route.tab === "process") return ["process"];
  if (route.name === "project" && route.tab === "metrics") return ["metrics"];
  return [];
}

const sent = new Set<LearningMark>();

/** Sets one learning mark in the background, once per app session; a mark that did not stand is
 * tried again the next time. A failure never reaches the page that set it. Input: the mark. */
export function markLearned(step: LearningMark): void {
  if (sent.has(step)) return;
  sent.add(step);
  void setLearningMark(step, true).then((result) => { if (result.kind !== "ok") sent.delete(step); });
}

/** Sets the learning marks for the page the main window shows, once the engine runs.
 * Inputs: the route and whether the engine runs. */
export function useLearningVisits(route: Route, running: boolean): void {
  const key = formatRoute(route);
  useEffect(() => {
    if (running) for (const mark of marksForRoute(route)) markLearned(mark);
  }, [key, running]);
}

/** Home's side panel: Get set up, or Learn CrazyAgent with its progress, Hide guide and the steps
 * under their topics. Inputs: the panel model and what Hide guide does. */
export function LearnPanel({ model, onHide }: { model: LearnPanelModel; onHide?: () => void }) {
  const hide = model.learning && onHide ? <Button variant="quiet" size="small" onClick={onHide}>Hide guide</Button> : undefined;
  return <Panel title={model.title} actions={hide} padded={false} className="setup-panel">
    {model.learning && <p className="secondary learn-progress">{model.done} of {model.total} done</p>}
    {model.groups.map((group, index) => <section key={group.heading ?? index}>
      {group.heading && <h3>{group.heading}</h3>}
      <ol>
        {group.items.map((item) => <li key={item.id}>
          <span className="row"><Icon name={item.icon} /><strong className="grow">{item.title}</strong>
            {item.done && <span className="done"><Icon name="circle-check" size={14} />Done</span>}</span>
          <p className="secondary">{item.body}</p>
          {!item.done && <span><a className={`btn${item.id === "harnesses" ? " quiet" : ""} small`} href={item.href}>{item.action}</a></span>}
        </li>)}
      </ol>
    </section>)}
  </Panel>;
}
