import { isMac } from "./ui/keys";

const PERMISSION_DENIED_MAC = "macOS didn't let CrazyAgent read or move this folder. Allow CrazyAgent in System Settings > Privacy & Security > Files and Folders, check the folder's permissions, then try again. Nothing was removed.";
const PERMISSION_DENIED_ELSEWHERE = "CrazyAgent couldn't read or move this folder: its permissions don't allow it. Check the folder's owner and permissions, then try again. Nothing was removed.";

/** The plain sentence for `home_permission_denied`: macOS privacy settings on macOS, the
 * folder's own permissions on Windows and Linux. Input: whether this is macOS, by default the
 * window's. Output: the sentence. */
export function permissionRefusal(mac: boolean = isMac()): string {
  return mac ? PERMISSION_DENIED_MAC : PERMISSION_DENIED_ELSEWHERE;
}
