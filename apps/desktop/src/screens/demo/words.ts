/** The Home entry: its title, its two sentences and its two actions, Try before a demo
 * exists and Open once it does. Every demo word here is the same in the TUI (DEC-153), which
 * keeps it as a constant in tui/screens/demo.rs; demo-words.test.mjs holds the two equal. */
export const DEMO_ENTRY = {
  title: "Try the demo",
  body: "Watch the whole loop on a small web page: the master plans, agents work, you answer their question and accept their change set. It runs for real on a pool you pick.",
  start: "Try the demo",
  open: "Open the demo",
} as const;

/** The cost note, shown first on the demo page, before any pool. */
export const DEMO_COST_NOTE = "The demo is a real run. Its master and agents spend the limits of the pool you pick, like any project, and any account in that pool may be used, since a pool moves to its next account when one runs out.";

/** What the demo makes and what it never does. */
export const DEMO_ABOUT = "CrazyAgent makes a project named CrazyAgent demo in your projects folder. Its agents work Sandboxed with no remote: nothing is pushed and nothing is downloaded.";

/** The pool list's lead line. */
export const DEMO_PICK = "Pick the pool the demo runs on. Nothing is picked for you.";

/** The create button. */
export const DEMO_CREATE = "Create the demo";

/** The line shown when the owner has no pool yet, and its action. */
export const DEMO_NO_POOL = "You have no pool yet. Create one with an account that is ready for agents, then come back.";
export const DEMO_NO_POOL_ACTION = "Create a pool";

/** The line shown in place of the pool list once the demo exists. */
export const DEMO_EXISTS = "Your demo project is ready. Open it to follow the guide.";

/** The mark a demo project carries on its card and head. */
export const DEMO_MARK = "Demo";

/** Why a pool cannot run the demo, by the engine's code, shown on that pool's row. */
export const DEMO_POOL_REASONS: Readonly<Record<string, string>> = {
  demo_harness_unavailable: "Its accounts run only on rcli, which has no live terminal or team agents yet.",
  demo_pool_not_ready: "No account in this pool is ready for agents.",
  demo_pool_missing: "This pool is gone.",
};

/** A pool's reason for a code this app has no sentence for; the code follows it. */
export const DEMO_POOL_UNKNOWN = "The demo can't run on this pool.";

/** What a refused Create the demo says, by the engine's code. */
export const DEMO_REFUSALS: Readonly<Record<string, string>> = {
  demo_exists: "Your demo project already exists. Open it, or delete it before making a new one.",
  demo_name_taken: "Your projects folder already holds a project or folder named CrazyAgent demo. Rename or move it, then try again.",
  demo_project_id_invalid: "This app sent a demo request the engine does not take. Update CrazyAgent, then try again.",
  demo_pool_missing: "That pool is gone. Pick another pool.",
  demo_pool_not_ready: "No account in that pool is ready for agents. Add a ChatGPT or Claude sign-in, or an OpenAI or Anthropic API key, or pick another pool.",
  demo_harness_unavailable: "That pool's accounts run only on rcli, which has no live terminal or team agents yet. Pick a pool with an account that runs Claude Code or Codex.",
  demo_platform_unqualified: "The demo runs only on macOS in this version.",
  demo_seed_failed: "CrazyAgent couldn't write the demo's files into your projects folder. Check that it can be written to, then try again.",
};

/** What Create the demo says when the engine's answer could not be confirmed, and when the
 * engine has no demo yet. */
export const DEMO_UNCONFIRMED = "We couldn't confirm the demo was made. Check your projects before trying again.";
export const DEMO_NOT_READY = "Not ready yet: this engine has no demo.";

/** The line the demo's page shows when the demo was made but the picked pool could not be
 * given to it, so its master has no account until the owner chooses the pool again. */
export const DEMO_POOL_UNASSIGNED = "The demo was made, but the pool you picked wasn't given to it. Choose the pool again in the demo's Settings, Pool, before you send the goal.";

/** The guide panel's title on the demo project's page. */
export const DEMO_GUIDE_TITLE = "Demo guide";

/** The guide's goal block label and its copy action. */
export const DEMO_GOAL_LABEL = "The goal to send";
export const DEMO_COPY_GOAL = "Copy goal";

/** The guide's steps in order: an id, a title and two sentences. The first four tick from the
 * engine's demo read; the last opens Delete everything and never ticks. */
export const DEMO_STEPS = [
  { id: "goal", title: "Give the master its goal", body: "Open the master on Conversation and send it the goal below. You talk to the master, and it plans the work." },
  { id: "agents", title: "Watch the agents work", body: "The master hands tasks to team agents. Agents shows who works on what." },
  { id: "question", title: "Answer the question", body: "An agent asked you something and waits for your answer. Answer it on Questions." },
  { id: "changes", title: "Accept the change set", body: "The agents' work lands as one change set. Review it on Changes and accept it." },
  { id: "delete", title: "Delete the demo", body: "When you are done, open Remove and pick Delete everything. The demo's folder goes to the Trash." },
] as const;

export type DemoStepId = typeof DEMO_STEPS[number]["id"];
