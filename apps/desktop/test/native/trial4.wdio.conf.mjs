import { fileURLToPath } from "node:url";
import { config as trial3 } from "./trial3.wdio.conf.mjs";

/** The trial 4 WebdriverIO config: the trial 3 config, its environment, fixture and checks,
 * running trial4.e2e.mjs, which replays the trial 3 steps with the trial 4 checks between them. */
export const config = { ...trial3, specs: [fileURLToPath(new URL("trial4.e2e.mjs", import.meta.url))] };
