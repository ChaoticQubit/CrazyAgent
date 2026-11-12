import { useEffect, useRef, useState } from "react";
import type { Result } from "../../api";
import { loadChanges } from "../../api-changes";
import type { ChangeSet } from "../../api-changes";

/** How long the Changes tab waits after one list reply before it reads the list again. */
export const LIST_POLL_MS = 5000;

/**
 * Keeps a project's change list read: one read in flight at a time, the next one `every`
 * milliseconds after the last reply, and every reply shown however slow it was. A reread
 * asked while a read is in flight runs once that read settles. Only a project change or
 * unmount drops a reply. Inputs: the project id, the wait between reads and the reader.
 * Output: the latest reply, or undefined before the first, and a reread action.
 */
export function useChangeList(
  projectId: string,
  every = LIST_POLL_MS,
  read: (projectId: string) => Promise<Result<ChangeSet[]>> = loadChanges,
): [Result<ChangeSet[]> | undefined, () => void] {
  const [list, setList] = useState<Result<ChangeSet[]>>();
  const reread = useRef<() => void>(() => undefined);
  useEffect(() => {
    let current = true;
    let reading = false;
    let again = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const next = (): void => {
      clearTimeout(timer);
      if (reading) {
        again = true;
        return;
      }
      reading = true;
      void read(projectId).then((result) => {
        reading = false;
        if (!current) return;
        setList(result);
        if (again) {
          again = false;
          next();
        } else timer = setTimeout(next, every);
      });
    };
    reread.current = next;
    next();
    return () => {
      current = false;
      clearTimeout(timer);
      reread.current = () => undefined;
    };
  }, [projectId, every, read]);
  return [list, () => reread.current()];
}
