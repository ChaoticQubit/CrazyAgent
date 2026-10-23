import { openLink } from "../../api-projects";
import { safeHref } from "./markdown";

/** Opens report links in the system browser through the engine bridge. Until that bridge
 * command exists, the link is copied so the owner can paste it into a browser.
 * Input: a notify handler. Output: the link handler for MarkdownView. */
export function useLinkOpener(notify: (message: string) => void): (href: string) => void {
  return (href: string) => {
    const safe = safeHref(href);
    if (!safe) return;
    void openLink(safe).then(async (result) => {
      if (result.kind === "ok") return;
      try {
        await navigator.clipboard.writeText(safe);
        notify("Link copied. Paste it into your browser.");
      } catch {
        notify(`Couldn't open the link. Copy it from Sources: ${safe}`);
      }
    });
  };
}
