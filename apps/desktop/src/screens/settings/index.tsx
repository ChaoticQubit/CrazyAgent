import { PageHeader, Tabs, formatRoute, type IconName, type SettingsTab } from "../../ui";
import type { SettingsAction } from "../../ui/router";
import { Accounts } from "./accounts";
import { GitHubSettings } from "./github";
import { Harnesses } from "./harnesses";
import { MemorySettings } from "./memory";
import { PackSettings } from "./pack";
import { PluginsAndSkills } from "./plugins";
import { Pools } from "./pools";
import { ProfileSettings } from "./profile";
import { ProjectsHomeSettings } from "./projects-home";

const TABS: { id: SettingsTab; label: string; icon: IconName }[] = [
  { id: "accounts", label: "Accounts", icon: "key-round" },
  { id: "pools", label: "Pools", icon: "layers" },
  { id: "harnesses", label: "Harnesses", icon: "terminal" },
  { id: "plugins", label: "Plugins and skills", icon: "puzzle" },
  { id: "pack", label: "Pack", icon: "boxes" },
  { id: "home", label: "Projects folder", icon: "folder" },
  { id: "github", label: "GitHub", icon: "git-branch" },
  { id: "memory", label: "Memory", icon: "brain" },
  { id: "profile", label: "Profile", icon: "user-round" },
];

/** Settings: the header, the tabs and the selected tab's panel.
 * Inputs: the tab and optional action ("add" opens Add account, "new" opens a new pool). */
export function Settings({ tab, action }: { tab: SettingsTab; action?: SettingsAction }) {
  return <>
    <PageHeader title="Settings" />
    <Tabs label="Settings sections" current={tab} tabs={TABS.map((item) => ({ ...item, href: formatRoute({ name: "settings", tab: item.id }) }))} />
    <div role="tabpanel" aria-labelledby={`tab-${tab}`}>
      {tab === "accounts" ? <Accounts adding={action === "add"} />
        : tab === "pools" ? <Pools creating={action === "new"} />
        : tab === "harnesses" ? <Harnesses />
        : tab === "plugins" ? <PluginsAndSkills />
        : tab === "pack" ? <PackSettings />
        : tab === "home" ? <ProjectsHomeSettings />
        : tab === "github" ? <GitHubSettings />
        : tab === "memory" ? <MemorySettings />
        : <ProfileSettings />}
    </div>
  </>;
}
