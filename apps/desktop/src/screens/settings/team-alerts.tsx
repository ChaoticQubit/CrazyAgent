import { loadTeamEvents } from "../../api-projects";
import type { TeamNotification } from "../../engine-client";
import { useAppSession, useLoad } from "../../state";
import { LoadError, NotReady, Panel, Skeleton } from "../../ui";
import { NotificationList, WakeRuleEditor } from "../team/wake-rules";
import "../team/team-graph.css";

/** Groups notifications by project, keeping each project's newest first and the projects in
 * the order of their newest notification. Input: the notifications, newest first. Output:
 * one entry per project. */
export function byProject(notifications: TeamNotification[]): { projectId: string; items: TeamNotification[] }[] {
  const groups = new Map<string, TeamNotification[]>();
  for (const item of notifications) groups.set(item.project_id, [...(groups.get(item.project_id) ?? []), item]);
  return [...groups].map(([projectId, items]) => ({ projectId, items }));
}

/** Settings, Team alerts: the app's own wake rule, which every project and agent uses until it
 * sets one, and the owner's notifications of every project. Output: the panel. */
export function TeamAlertsSettings() {
  const session = useAppSession();
  const names = new Map(session.projects.kind === "ok" ? session.projects.value.map((project) => [project.id, project.name]) : []);
  const [events, reload] = useLoad(loadTeamEvents);
  if (events.kind === "loading") return <Skeleton shape="card" />;
  if (events.kind === "not_ready") return <NotReady what="Team alerts" />;
  if (events.kind === "error") return <LoadError message={`Couldn't load team alerts. ${events.message}`} onRetry={reload} />;
  const groups = byProject(events.value.notifications);
  return <Panel title="Wake the master">
    <p className="tg-terminal-note">Sub-agent events wake each project's master on their own. Projects and agents can set their own rule on the Agents tab.</p>
    <WakeRuleEditor target={{ level: "app" }} own={events.value.rule} effective={events.value.effective} fallback="shipped" reload={reload} />
    {groups.map((group) => <div key={group.projectId}>
      <h3>{names.get(group.projectId) ?? "A removed project"}</h3>
      <NotificationList projectId={group.projectId} notifications={group.items} reload={reload} />
    </div>)}
  </Panel>;
}
