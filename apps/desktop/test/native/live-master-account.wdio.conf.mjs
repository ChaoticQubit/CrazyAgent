import { fileURLToPath } from "node:url";
import { config as masterSwitch } from "./master-switch.wdio.conf.mjs";

export const config = {
  ...masterSwitch,
  specs: [fileURLToPath(new URL("live-master-account.e2e.mjs", import.meta.url))],
};
