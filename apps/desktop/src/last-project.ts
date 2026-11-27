const LAST_PROJECT_KEY = "crazyagent.last-project";
const PROJECT_ID_MAX = 256;

/** Reads the project whose page the owner opened last, as kept in this app, so app start can
 * load its views in the background (DEC-187). Output: the project id; null when none was
 * kept, storage is unavailable or it holds something else. */
export function readLastProject(): string | null {
  try {
    const value = window.localStorage.getItem(LAST_PROJECT_KEY);
    return value && value.length <= PROJECT_ID_MAX ? value : null;
  } catch {
    return null;
  }
}

/** Remembers the project whose page the owner opened. Input: the project id. Output: none. */
export function saveLastProject(id: string): void {
  if (!id || id.length > PROJECT_ID_MAX) return;
  try {
    window.localStorage.setItem(LAST_PROJECT_KEY, id);
  } catch {
    return;
  }
}
