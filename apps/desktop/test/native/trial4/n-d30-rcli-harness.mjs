const MASTER_ONLY = "Limited control: runs only as a CrazyAgent master";

/** Judges where Settings, Harnesses lists RCLI: Ready, or Limited control as a harness that
 * runs only as the master, the two places it can be picked as the project master from.
 * Input: the row { group, status }. Output: null when RCLI may be chosen from there, else
 * what is wrong in the owner's words. */
export function rcliProblem(row) {
  if (row.group === "Ready" || (row.group === "Limited control" && row.status === MASTER_ONLY)) return null;
  return `RCLI is listed under ${row.group} (${row.status}), not Ready or as master only`;
}

/** Reads the RCLI row of Settings, Harnesses: the group it is listed under and its status.
 * Output: { group, status } or null while the catalog has no RCLI row. */
function rcliRow() {
  return browser.execute(() => {
    for (const section of document.querySelectorAll("main .set-group")) {
      for (const row of section.querySelectorAll(".set-row")) {
        if (row.dataset.setting === "RCLI") return { group: section.querySelector("h3").textContent.replace(/ \(\d+\)$/, "").trim(), status: row.querySelector(".set-ctl")?.textContent?.trim() ?? "" };
      }
    }
    return null;
  });
}

/** Reads the RCLI choice of the project's Master harness list without changing the team.
 * Output: { listed, disabled, label } or null before the list shows. */
function rcliChoice() {
  return browser.execute(() => {
    const select = document.getElementById("ps-master-harness");
    if (!select) return null;
    const option = [...select.options].find((entry) => entry.value === "rcli");
    return { listed: Boolean(option), disabled: Boolean(option?.disabled), label: option?.textContent?.trim() ?? "" };
  });
}

export const checks = [{
  id: "N-D30",
  op: ["OP-51"],
  title: "The installed RCLI harness is found and can be chosen for the master",
  why: "owner: an installed RCLI harness is not discovered, so it cannot be chosen",
  after: "end",
  run: async (kit) => {
    await kit.go("#/settings/harnesses");
    const row = JSON.parse(await kit.waitFor(async () => {
      const seen = await rcliRow();
      return { done: seen !== null, seen: JSON.stringify(seen) };
    }, kit.UI_MS, "Settings, Harnesses listed no RCLI row"));
    const problem = rcliProblem(row);
    if (problem) throw new Error(problem);
    await kit.screen();
    await kit.go(`#/projects/${encodeURIComponent(kit.project())}/settings`);
    const choice = JSON.parse(await kit.waitFor(async () => {
      const seen = await rcliChoice();
      return { done: seen !== null, seen: JSON.stringify(seen) };
    }, kit.UI_MS, "the project's Master harness list did not show"));
    if (!choice.listed) throw new Error("the Master harness list has no RCLI choice");
    if (choice.disabled) throw new Error(`the Master harness list shows RCLI (${choice.label}) but it cannot be picked`);
    await kit.inView("#ps-master-harness");
    await kit.screen();
    return `RCLI listed under ${row.group} (${row.status}); the Master harness list offers ${choice.label}`;
  },
}];
