/** A changed file as `ide.change_sets` lists it: its path in the worktree, status and marks. */
export interface MarkedFile {
  path: string;
  status: string;
  marks: string[];
}

/** A change set as `ide.change_sets` lists it, with only the fields the trust guard reads. */
export interface MarkedSet {
  id: string;
  agent: string;
  state: string;
  head: string;
  worktree: string;
  files: MarkedFile[];
}

/** The mark the engine puts on a file the IDE reads as its settings. */
export const IDE_CONFIG = "ide_config";

/** The words a decorated settings file shows, the same as the desktop's mark. */
export const IDE_CONFIG_NOTE = "IDE settings: changes how the IDE behaves in this project";

const OPEN = new Set(["pending", "conflict", "updating"]);
const NAMED_MAX = 3;
const UNSAFE = /[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/g;

/** Engine or agent text as one plain line: control and direction-changing characters
 * become a replacement mark. Input: the text. Output: the line. */
function plain(text: string): string {
  return text.replace(UNSAFE, "�");
}

/** The settings files an open change set changed, as the files in the agent's worktree:
 * every file with the IDE settings mark that still exists. Input: the listed sets. Output:
 * a map from each file's path on disk to the agent that changed it. */
export function ideConfigFiles(sets: MarkedSet[]): Map<string, string> {
  const files = new Map<string, string>();
  for (const set of sets) {
    if (!OPEN.has(set.state)) continue;
    const root = `${set.worktree.replace(/\/+$/, "")}/`;
    for (const file of set.files) {
      if (file.marks.includes(IDE_CONFIG) && file.status !== "deleted") files.set(`${root}${file.path}`, set.agent);
    }
  }
  return files;
}

/** The one warning for an open change set that changes IDE settings, naming at most three of
 * its files. Input: the set. Output: the sentence, or null when it changes none. */
export function ideConfigWarning(set: MarkedSet): string | null {
  if (!OPEN.has(set.state)) return null;
  const paths = set.files.filter((file) => file.marks.includes(IDE_CONFIG)).map((file) => plain(file.path));
  if (paths.length === 0) return null;
  const more = paths.length > NAMED_MAX ? ` and ${paths.length - NAMED_MAX} more` : "";
  return `${plain(set.agent)} changed IDE settings: ${paths.slice(0, NAMED_MAX).join(", ")}${more}. Check them before you accept: they change how the IDE behaves in this project.`;
}

/** The key that says a set's warning was shown for this head, so a new head warns again.
 * Input: the set. Output: the key. */
export function warningKey(set: MarkedSet): string {
  return `${set.id}:${set.head}`;
}
