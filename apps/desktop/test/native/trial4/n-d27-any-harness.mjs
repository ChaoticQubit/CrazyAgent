import { shown, texts } from "../settings-steps.mjs";
import { UI_MS } from "./n-d8-master-reply.mjs";

/** The note Team settings shows for a plan with no harness chosen (DEC-096 addendum 3). */
export const ANY_NOTE = "Any harness, no limit.";

/** The Overview line for a plan with no harness chosen, for example "Any harness, up to 63 agents". */
export const ANY_TOTAL = /^Any harness, up to (\d+) agents?$/;

/** Lists the harnesses the engine lets the master start agents of: every runnable row of the
 * team status that is not the master's own. Input: the team status. Output: the distinct
 * harness ids in status order; throws when the status has no harness list. */
export function agentHarnesses(status) {
  if (!Array.isArray(status?.harnesses)) throw new Error(`the team status has no harness list: ${JSON.stringify(status).slice(0, 300)}`);
  return [...new Set(status.harnesses.filter((row) => row.role !== "master" && row.runnable).map((row) => row.harness))];
}

/** Reads the project's team status through the app's own command. Input: the project id.
 * Output: the status; throws when the engine refused. */
async function teamStatus(id) {
  const answer = await browser.executeAsync((id, done) => {
    window.__TAURI_INTERNALS__.invoke("desktop_team_status", { id }).then(done, (failure) => done({ failed: String(failure) }));
  }, id);
  if (answer?.failed) throw new Error(`desktop_team_status failed: ${answer.failed}`);
  return answer?.value ?? answer;
}

export const checks = [{
  id: "N-D27", op: ["OP-34"], title: "No harness chosen reads Any harness with no limit, and the master can start agents of several harnesses",
  why: "owner: \"No harness chosen shows as zero harnesses, only the master, not as unlimited\"",
  after: "add-project",
  run: async (kit) => {
    const id = kit.project();
    await kit.go(`#/projects/${encodeURIComponent(id)}/settings?section=team`);
    await kit.until(() => shown("#ps-master-harness"), UI_MS, "Team settings never showed the team editor");
    const notes = await texts(".pj-help");
    const note = notes.find((line) => line.startsWith("Any harness") || line.startsWith("No sub-agents"));
    if (note === undefined) throw new Error("the trial project's plan names harnesses; this check needs a project with no harness chosen");
    if (!note.startsWith(ANY_NOTE)) throw new Error(`Team settings reads "${note}", not "${ANY_NOTE}"`);
    await kit.screen();
    await kit.tab("overview");
    let total = "";
    await kit.until(async () => {
      total = (await texts("dd.pj-secondary")).find((line) => ANY_TOTAL.test(line) || line.startsWith("No sub-agents")) ?? "";
      return total !== "";
    }, UI_MS, "the Overview never showed the team's agent line");
    if (!ANY_TOTAL.test(total)) throw new Error(`the Overview reads "${total}" for a plan with no harness chosen`);
    await kit.screen();
    const harnesses = agentHarnesses(await teamStatus(id));
    if (harnesses.length < 2) throw new Error(`the engine lets the master start agents of ${harnesses.length} harness (${harnesses.join(", ") || "none"}), not several`);
    return `Team settings: "${note}"; Overview: "${total}"; the master can start agents of ${harnesses.length} harnesses: ${harnesses.join(", ")}`;
  },
}];
