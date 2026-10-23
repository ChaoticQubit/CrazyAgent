import type { ReactNode } from "react";
import type { HarnessEntry, Result } from "../../api";
import { BrandMark, Icon, StatusBadge, type Tone } from "../../ui";
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

/** The CrazyAgent mark without its name, as decoration beside an empty state or a master reply.
 * Input: large for an empty state. Output: the mark, hidden from screen readers. */
export function ProjectMark({ large = false }: { large?: boolean }) {
  return <span className={`pj-mark${large ? " large" : ""}`} aria-hidden="true"><BrandMark large={large} /></span>;
}

/** An empty state on the project page: the mark, a heading and one plain line.
 * Inputs: the heading and the line. Output: the empty state. */
export function ProjectEmpty({ title, children }: { title: string; children: ReactNode }) {
  return <div className="pj-empty"><ProjectMark large /><h3>{title}</h3><p className="pj-secondary">{children}</p></div>;
}
