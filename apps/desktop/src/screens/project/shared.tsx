import type { HarnessEntry, Result } from "../../api";
import { Icon, StatusBadge, type Tone } from "../../ui";
import type { PoolChoice, ProjectContext, ResearchJob, StoredSetup, TeamStatus } from "../../api-projects";

/** The latest live team read and when it arrived, in milliseconds. */
export interface TeamRead {
  result: Result<TeamStatus> | undefined;
  at: number;
}

export interface ProjectData {
  id: string;
  name: string;
  setup: Result<StoredSetup | null> | undefined;
  research: Result<{ project_id: string; jobs: ResearchJob[] }> | undefined;
  context: Result<ProjectContext> | undefined;
  pools: Result<PoolChoice[]> | undefined;
  harnesses: Result<HarnessEntry[]> | undefined;
  assigned: PoolChoice | null;
  now: number;
  navigate: (path: string) => void;
  notify: (message: string) => void;
  reload: () => void;
  reloadPools: () => void;
  team: TeamRead;
  reloadTeam: () => void;
}

/** Reads a result's value. Input: an engine result. Output: its value, or null. */
export function valueOf<T>(result: Result<T> | undefined): T | null {
  return result?.kind === "ok" ? result.value : null;
}

/** Shows the plain "Not ready yet" state for an engine part that isn't in this build.
 * Input: what is not ready. Output: the notice. */
export function NotReady({ what }: { what: string }) {
  return <p className="pj-note warn pj-inline" role="status"><Icon name="clock" size={14} /><span>Not ready yet: {what}</span></p>;
}

/** Shows a status word with its tone; color never carries meaning alone.
 * Inputs: the word and its tone. Output: the badge. */
export function Badge({ word, tone }: { word: string; tone: Tone }) {
  return <StatusBadge word={word} tone={tone} />;
}
