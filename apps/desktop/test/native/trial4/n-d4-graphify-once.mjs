import { click, shown } from "../settings-steps.mjs";
import { graphifyVerdict, tidy } from "../trial4-accounts.mjs";

/** Reads every Tools row and pack entry of Settings, Pack. Output: rows { section, title,
 * words, on, id }, on and id being the switch's state and id, or null with no switch. */
function packRows() {
  return browser.execute(() => [...document.querySelectorAll("main .set-group .set-row")].filter((node) => node.getClientRects().length).map((row) => {
    const toggle = row.querySelector("button[role=\"switch\"]");
    return {
      section: row.closest(".set-group").querySelector("h3").firstChild.textContent.trim(),
      title: row.dataset.setting ?? "",
      words: [...row.querySelectorAll(".badge")].map((badge) => badge.textContent.trim()),
      on: toggle ? toggle.getAttribute("aria-checked") === "true" : null,
      id: toggle?.id ?? null,
    };
  }));
}

/** Flips one pack switch as the owner clicks it and waits for the engine's state to show.
 * Inputs: the kit, the switch selector and the state it must reach. */
async function flip(kit, selector, on) {
  await click(selector);
  await kit.until(async () => await $(selector).getAttribute("aria-checked") === String(on), kit.UI_MS, `OP-49: the Graphify pack switch did not turn ${on ? "on" : "off"}`);
}

export const checks = [{
  id: "N-D4",
  op: ["OP-49"],
  title: "Graphify shows once, reads installed when present, and, on a fixture, its pack switch turns off and on",
  why: "owner: \"since it's default, I couldn't even uncheck the box\"",
  after: "end",
  run: async (kit) => {
    await kit.go("#/settings/pack");
    await kit.until(async () => await shown("//section[contains(@class, \"set-group\")][.//h3[normalize-space(.)=\"Tools\"]]") && (await packRows()).some((row) => row.section !== "Tools"), kit.UI_MS, "Settings, Pack never showed its tools and pack entries");
    const { tool, entry } = graphifyVerdict(await packRows());
    if (!/^pack-[a-z0-9_-]+$/u.test(entry.id ?? "")) throw new Error(`the Graphify pack switch has an unexpected id ${entry.id}`);
    const selector = `#${entry.id}`;
    await kit.inView(selector);
    await kit.screen();
    const seen = `${tool.title} reads ${tool.words.join(", ")} once among the tools; ${entry.title} shows once in ${entry.section}`;
    if (kit.MODE !== "fixture") return `${seen} and its switch reads ${entry.on ? "on" : "off"}; a live run leaves the owner's switch alone`;
    return tidy(async () => {
      await flip(kit, selector, !entry.on);
      await kit.screen();
      await flip(kit, selector, entry.on);
      await kit.screen();
      return `${seen} and its switch went ${entry.on ? "off and back on" : "on and back off"}`;
    }, async () => {
      if (await $(selector).getAttribute("aria-checked") !== String(entry.on)) await flip(kit, selector, entry.on);
    });
  },
}];
