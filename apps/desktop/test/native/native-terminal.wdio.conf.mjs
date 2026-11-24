import { fileURLToPath } from "node:url";
import { config as masterSwitch } from "./master-switch.wdio.conf.mjs";
import { displays, testWindow } from "./test-window.mjs";

/** The test window on the first 1x display that holds it, so it never opens on the owner's
 * 2x built-in display, sized to that display's visible area ([`testWindow`]). */
const shown = testWindow(displays(), { scaleOne: true });

const [[service, options]] = masterSwitch.services;

export const config = {
  ...masterSwitch,
  specs: [fileURLToPath(new URL("native-terminal.e2e.mjs", import.meta.url))],
  services: [[service, { ...options, env: { ...options.env, CRAZYAGENT_E2E_WINDOW_AT: shown.place } }]],
  before: async function () {
    await globalThis.browser.setWindowSize(shown.width, shown.height);
  },
};
