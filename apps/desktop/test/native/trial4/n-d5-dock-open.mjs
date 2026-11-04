import { engineVerdict } from "../trial4-accounts.mjs";

const ENGINE_MS = 60000;

/** Counts the project cards Home draws. Output: the count. */
function projectCards() {
  return browser.execute(() => [...document.querySelectorAll("main .project-card")].filter((node) => node.getClientRects().length).length);
}

export const checks = [{
  id: "N-D5",
  op: ["OP-43"],
  title: "The app opened from the Dock finds its engine",
  why: "owner: \"it says engine is not running, cannot connect to engine\"",
  after: "end",
  run: async (kit) => {
    if (typeof kit.relaunch !== "function") {
      throw new Error("not run: the trial kit has no relaunch() that quits the app and opens it as the Dock does (open -a, no CRAZYAGENT_DESKTOP_SOCKET)");
    }
    await kit.relaunch();
    await kit.go("#/home");
    await kit.until(async () => (await projectCards()) > 0, ENGINE_MS, "Home listed no project").catch(() => undefined);
    const text = await kit.screen();
    return engineVerdict({ text, projects: await projectCards() });
  },
}];
