import { config as baseline } from "../../wdio.conf.mjs";
import { fileURLToPath } from "node:url";

export const config = {
  ...baseline,
  specs: [fileURLToPath(new URL("conversation.e2e.mjs", import.meta.url))],
};
