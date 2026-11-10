import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { config as masterSwitch } from "./master-switch.wdio.conf.mjs";

const [[service, options]] = masterSwitch.services;

export const config = {
  ...masterSwitch,
  specs: [fileURLToPath(new URL("ide-window.e2e.mjs", import.meta.url))],
  mochaOpts: { ...masterSwitch.mochaOpts, timeout: 1800000, grep: process.env.IDE_WINDOW_GREP ?? "" },
  services: [[service, { ...options, env: { ...options.env, CRAZYAGENT_E2E_IDE_PROBE: join(process.env.MS_EVIDENCE, "ide-refusal.json") } }]],
};
