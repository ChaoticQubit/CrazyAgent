import { config as baseline } from "../../wdio.conf.mjs";
import { fileURLToPath } from "node:url";

export const config = {
  ...baseline,
  specs: [fileURLToPath(new URL("model-access.e2e.mjs", import.meta.url))],
  mochaOpts: { timeout: 100000, retries: 0 },
};
