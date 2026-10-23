import type { EngineResult, HarnessEntry, PoolChoice, ProjectContext, ResearchJob, StoredSetup } from "../../api-projects";

export interface ProjectData {
  id: string;
  name: string;
  setup: EngineResult<StoredSetup | null> | undefined;
  research: EngineResult<{ project_id: string; jobs: ResearchJob[] }> | undefined;
  context: EngineResult<ProjectContext> | undefined;
  pools: EngineResult<PoolChoice[]> | undefined;
  harnesses: EngineResult<HarnessEntry[]> | undefined;
  assigned: PoolChoice | null;
  now: number;
  navigate: (path: string) => void;
  notify: (message: string) => void;
  reload: () => void;
  reloadPools: () => void;
}

/** Reads a result's value. Input: an engine result. Output: its value, or null. */
export function valueOf<T>(result: EngineResult<T> | undefined): T | null {
  return result?.kind === "ok" ? result.value : null;
}

/** Shows the plain "Not ready yet" state for an engine part that isn't in this build.
 * Input: what is not ready. Output: the notice. */
export function NotReady({ what }: { what: string }) {
  return <p className="pj-note warn" role="status">Not ready yet: {what}</p>;
}

/** Shows a status word with its tone; color never carries meaning alone.
 * Inputs: the word and its tone. Output: the badge. */
export function Badge({ word, tone }: { word: string; tone: string }) {
  return <span className={`pj-badge t-${tone}`}>{word}</span>;
}
