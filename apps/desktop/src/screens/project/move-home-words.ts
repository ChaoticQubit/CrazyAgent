import { isMac } from "../../ui/keys";

const UNREAD_MAC = "CrazyAgent couldn't read this folder. Anything in it only in iCloud is downloaded first, rebuildable folders too, which can be slow.";
const UNREAD_ELSEWHERE = "CrazyAgent couldn't read this folder.";

/** What the move dialog says when the folder read failed: on macOS that iCloud-only items and
 * rebuildable folders are downloaded first; on Windows and Linux, which have no iCloud-only items,
 * only that the read failed. Input: whether this is macOS, by default the window's.
 * Output: the line. */
export function moveUnreadLine(mac: boolean = isMac()): string {
  return mac ? UNREAD_MAC : UNREAD_ELSEWHERE;
}
