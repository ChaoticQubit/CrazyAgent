import { loadTeamEvents } from "../../api-projects";
import type { TeamNotification } from "../../engine-client";
import { systemNotifications, systemNotificationsLine } from "../../notify";
import { useAppSession, useLoad } from "../../state";
import { EmptyState, LoadError, NotReady, SettingSection, Skeleton } from "../../ui";
import { NotificationList, WakeRuleEditor } from "../team/wake-rules";
import "../project/projects.css";
import "../team/team-graph.css";

const ALERTS_TOP = <div className="settings-top"><div>
  <h2>Team alerts</h2>
  <p>What wakes each project's master, and what your projects have told you.</p>
</div></div>;

/** Groups notifications by project, keeping each project's newest first and the projects in
 * the order of their newest notification. Input: the notifications, newest first. Output:
 * one entry per project. */
export function byProject(notifications: TeamNotification[]): { projectId: string; items: TeamNotification[] }[] {
  const groups = new Map<string, TeamNotification[]>();
  for (const item of notifications) groups.set(item.project_id, [...(groups.get(item.project_id) ?? []), item]);
  return [...groups].map(([projectId, items]) => ({ projectId, items }));
}

/** Settings, Team alerts: the title in every state, the app's own wake rule, which every
 * project and agent uses until it sets one, whether macOS shows question notifications, and one
 * section of notifications per project, or No alerts when there are none. Output: the tab. */
export function TeamAlertsSettings() {
  const session = useAppSession();
  const names = new Map(session.projects.kind === "ok" ? session.projects.value.map((project) => [project.id, project.name]) : []);
  const [events, reload] = useLoad(loadTeamEvents);
  const [system] = useLoad(systemNotifications);
  const systemLine = systemNotificationsLine(system.kind === "loading" ? undefined : system);
  if (events.kind === "loading") return <>{ALERTS_TOP}<Skeleton shape="card" /></>;
  if (events.kind === "not_ready") return <>{ALERTS_TOP}<NotReady what="Team alerts" /></>;
  if (events.kind === "error") return <>{ALERTS_TOP}<LoadError message={`Couldn't load team alerts. ${events.message}`} onRetry={reload} /></>;
  const groups = byProject(events.value.notifications);
  return <>
    {ALERTS_TOP}
    <div className="pj set-groups">
      <SettingSection title="Wake the master" about="Sub-agent events wake each project's master on their own. Projects and agents can set their own rule on the Agents tab.">
        <div className="set-body">
          {systemLine && <p className="tg-terminal-note" role="note">{systemLine}</p>}
          <WakeRuleEditor target={{ level: "app" }} own={events.value.rule} version={events.value.rule_version} effective={events.value.effective} fallback="shipped" reload={reload} />
        </div>
      </SettingSection>
      {groups.length === 0 && <EmptyState compact title="No alerts">Notifications from your projects show up here.</EmptyState>}
      {groups.map((group) => <SettingSection key={group.projectId} title={names.get(group.projectId) ?? "A removed project"}>
        <div className="set-body"><NotificationList projectId={group.projectId} notifications={group.items} reload={reload} /></div>
      </SettingSection>)}
    </div>
  </>;
}
