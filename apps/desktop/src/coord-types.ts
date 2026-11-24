/** A team role whose skills the owner chooses: the master, a lead or a worker. */
export type SkillRole = "master" | "lead" | "worker";

/** Which approved skills each team role gets: null is All approved skills, a list is Chosen
 * skills, and an empty list gives that role no skills. */
export interface RoleSkills {
  master: string[] | null;
  lead: string[] | null;
  worker: string[] | null;
}

/** How every agent terminal of a project draws, whatever harness runs in it (DEC-141): Classic
 * on the normal screen, so the app's view scrolls with the wheel and a plain drag selects, or
 * Fullscreen, the harness's own view, which takes the mouse, the default. */
export type TerminalDisplay = "classic" | "fullscreen";

/** Whether the first turn of each task of a team agent carries the Task-start reminder: the
 * context-mode tools to load and the approved skills to check for the task. */
export type TaskStart = "on" | "off";

/** How many heavy commands of a project run at once through its heavy lane (F28): 1, 2, 3 or
 * 4 at a time, or No limit. */
export type HeavyLane = "one" | "two" | "three" | "four" | "no_limit";

/** A named thing a project's heavy commands share, such as a local model or a database, and
 * how many commands may use it at once, 1 to 8. */
export interface SharedResource {
  name: string;
  slots: number;
}

/** Whether an agent working in its own clone reads, at its next task, which of its edits it
 * left uncommitted and did not land; the owner is told either way. */
export type UncommittedReminder = "on" | "off";
