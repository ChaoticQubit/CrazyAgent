import { useLayoutEffect, type ReactNode } from "react";
import { PageHeader, formatRoute, type SettingsTab } from "../../ui";
import { SETTINGS_TABS, type SettingsAction } from "../../ui/router";
import { Accounts } from "./accounts";
import { GitHubSettings } from "./github";
import { Harnesses } from "./harnesses";
import { MemorySettings } from "./memory";
import { PackSettings } from "./pack";
import { PluginsAndSkills } from "./plugins";
import { Pools } from "./pools";
import { ProcessSettings } from "../process";
import { ProfileSettings } from "./profile";
import { ProjectsHomeSettings } from "./projects-home";
import { SandboxSettingsTab } from "./sandbox";
import { TeamAlertsSettings } from "./team-alerts";
import { UpdatesSettings } from "./updates";
import "../project/projects.css";
import "./settings.css";

interface SettingsGroup {
  title: string;
  items: { id: SettingsTab; label: string }[];
}

export const SETTINGS_GROUPS: SettingsGroup[] = [
  { title: "Access", items: [
    { id: "accounts", label: "Accounts" }, { id: "pools", label: "Pools" },
    { id: "github", label: "GitHub" }, { id: "profile", label: "Profile" },
  ] },
  { title: "Agents", items: [
    { id: "harnesses", label: "Harnesses" }, { id: "plugins", label: "Plugins and skills" },
    { id: "process", label: "Process" }, { id: "pack", label: "Pack" }, { id: "memory", label: "Memory" },
  ] },
  { title: "App", items: [
    { id: "home", label: "Projects folder" }, { id: "sandbox", label: "Sandbox" },
    { id: "alerts", label: "Team alerts" }, { id: "updates", label: "Updates" },
  ] },
];

/** Refuses a Settings list that leaves out, repeats or invents a section, so an edit cannot
 * lose one from view. Inputs: the groups and every section there is. Throws when they differ. */
export function assertEverySection(groups: SettingsGroup[], tabs: readonly SettingsTab[]): void {
  const listed = groups.flatMap((group) => group.items.map((item) => item.id));
  const same = listed.length === tabs.length && new Set(listed).size === tabs.length && tabs.every((tab) => listed.includes(tab));
  if (!same) throw new Error(`the Settings list must name every section once: ${listed.join(", ")}`);
}

assertEverySection(SETTINGS_GROUPS, SETTINGS_TABS);

/** The Settings page's frame as the approved design draws it: the title, then the sections
 * listed in groups beside the open section. A narrow window puts the list above it, and a
 * newly opened section starts at its top. Inputs: the open section and its panel. Output: the
 * frame. */
export function SettingsFrame({ tab, children }: { tab: SettingsTab; children: ReactNode }) {
  useLayoutEffect(() => {
    document.getElementById("main")?.scrollTo({ top: 0, behavior: "instant" });
  }, [tab]);
  return <>
    <PageHeader title="Settings" />
    <div className="settings-split">
      <nav className="settings-nav" aria-label="Settings sections">
        {SETTINGS_GROUPS.map((group) => <div key={group.title} role="group" aria-labelledby={`settings-group-${group.title}`}>
          <p className="settings-nav-title" id={`settings-group-${group.title}`}>{group.title}</p>
          {group.items.map((item) => <a key={item.id} className="settings-nav-item" href={formatRoute({ name: "settings", tab: item.id })}
            aria-current={item.id === tab ? "page" : undefined}>{item.label}</a>)}
        </div>)}
      </nav>
      <div className="settings-content">{children}</div>
    </div>
  </>;
}

/** Settings: the frame around the selected section's panel.
 * Inputs: the tab, optional action ("add" opens Add account, "new" opens a new pool,
 * "sign_in" opens the sign-in of the given account) and the skill whose detail is open. */
export function Settings({ tab, action, account, skill }: { tab: SettingsTab; action?: SettingsAction; account?: string; skill?: string }) {
  return <SettingsFrame tab={tab}>
    {tab === "accounts" ? <Accounts adding={action === "add"} signingIn={action === "sign_in" ? account : undefined} />
      : tab === "pools" ? <Pools creating={action === "new"} />
      : tab === "harnesses" ? <Harnesses />
      : tab === "plugins" ? <PluginsAndSkills skill={skill} />
      : tab === "process" ? <ProcessSettings />
      : tab === "pack" ? <PackSettings />
      : tab === "home" ? <ProjectsHomeSettings />
      : tab === "github" ? <GitHubSettings />
      : tab === "sandbox" ? <SandboxSettingsTab />
      : tab === "memory" ? <MemorySettings />
      : tab === "alerts" ? <TeamAlertsSettings />
      : tab === "updates" ? <UpdatesSettings />
      : <ProfileSettings />}
  </SettingsFrame>;
}
