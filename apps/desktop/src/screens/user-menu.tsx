import { useEffect, useRef, useSyncExternalStore } from "react";
import { Dialog } from "../ui";
import { formatRoute, type Route, type UserMenuTab } from "../ui/router";
import { USER_MENU_ITEMS } from "../ui/sidebar-foot";
import { GitHubSettings } from "./settings/github";
import { ProfileSettings } from "./settings/profile";
import { ProjectsHomeSettings } from "./settings/projects-home";
import { UpdatesSettings } from "./settings/updates";
import "./settings/settings.css";

let shown: UserMenuTab | null = null;
const listeners = new Set<() => void>();

function set(next: UserMenuTab | null): void {
  if (shown === next) return;
  shown = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

/** Opens a user menu dialog over the current page (DEC-151). Input: its tab. Output: none. */
export function openUserDialog(tab: UserMenuTab): void {
  set(tab);
}

/** Closes the open user menu dialog; the page under it stays as it was. Output: none. */
export function closeUserDialog(): void {
  set(null);
}

/** The user menu dialog open now. Output: its tab, or null. */
export function userDialog(): UserMenuTab | null {
  return shown;
}

/** Follows the open user menu dialog. Output: its tab or null, rendering again on a change. */
export function useUserDialog(): UserMenuTab | null {
  return useSyncExternalStore(subscribe, userDialog, userDialog);
}

/** One user menu section as a dialog: the menu item's words as the title and the section's
 * own panel inside, its heading left to the dialog. Inputs: the tab and the close action. */
export function UserMenuDialog({ tab, onClose }: { tab: UserMenuTab; onClose: () => void }) {
  return <Dialog open title={USER_MENU_ITEMS[tab].label} onClose={onClose} wide>
    <div className="user-dialog">
      {tab === "profile" ? <ProfileSettings onClose={onClose} />
        : tab === "updates" ? <UpdatesSettings />
        : tab === "github" ? <GitHubSettings />
        : <ProjectsHomeSettings />}
    </div>
  </Dialog>;
}

/** The open user menu dialog over the shell. A deep link to one of the four settings routes
 * opens its dialog and puts the address the owner was on back, a project section included, so
 * Back and every hash reader see that page; a move to another page closes the dialog. Inputs:
 * the tab a deep link asks for, if any, the page under it and that page's address. */
export function UserDialogLayer({ asked, under, back = formatRoute(under) }: { asked: UserMenuTab | null; under: Route; back?: string }) {
  const open = useUserDialog();
  const page = formatRoute(under);
  const shown = useRef(page);
  useEffect(() => {
    if (asked === null) return;
    openUserDialog(asked);
    window.history.replaceState(null, "", back);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  }, [asked]);
  useEffect(() => {
    if (shown.current === page) return;
    shown.current = page;
    if (asked === null) closeUserDialog();
  }, [page]);
  const tab = asked ?? open;
  return tab === null ? null : <UserMenuDialog tab={tab} onClose={closeUserDialog} />;
}
