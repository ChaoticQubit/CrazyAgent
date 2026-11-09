import type { ReactNode } from "react";
import { PageHeader, Tabs, formatRoute, type IconName, type SettingsTab } from "../../ui";
import type { SettingsAction } from "../../ui/router";
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
import { UsageSettings } from "./usage";
import "./settings.css";

const TABS: { id: SettingsTab; label: string; icon: IconName }[] = [
  { id: "accounts", label: "Accounts", icon: "key-round" },
  { id: "pools", label: "Pools", icon: "layers" },
  { id: "harnesses", label: "Harnesses", icon: "terminal" },
  { id: "plugins", label: "Plugins and skills", icon: "puzzle" },
  { id: "process", label: "Process", icon: "list-checks" },
  { id: "pack", label: "Pack", icon: "boxes" },
  { id: "home", label: "Projects folder", icon: "folder" },
  { id: "github", label: "GitHub", icon: "git-branch" },
  { id: "sandbox", label: "Sandbox", icon: "shield-check" },
  { id: "memory", label: "Memory", icon: "brain" },
  { id: "usage", label: "Usage", icon: "trending-up" },
  { id: "alerts", label: "Team alerts", icon: "inbox" },
  { id: "profile", label: "Profile", icon: "user-round" },
  { id: "updates", label: "Updates", icon: "refresh-cw" },
];

/** The Settings page's frame: the title, the section list and the open section. Inputs: the
 * open section and its panel. Output: the frame. */
export function SettingsFrame({ tab, children }: { tab: SettingsTab; children: ReactNode }) {
  return <>
    <PageHeader title="Settings" />
    <Tabs label="Settings sections" current={tab} tabs={TABS.map((item) => ({ ...item, href: formatRoute({ name: "settings", tab: item.id }) }))} />
    <div role="tabpanel" aria-labelledby={`tab-${tab}`}>{children}</div>
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
      : tab === "usage" ? <UsageSettings />
      : tab === "alerts" ? <TeamAlertsSettings />
      : tab === "updates" ? <UpdatesSettings />
      : <ProfileSettings />}
  </SettingsFrame>;
}
