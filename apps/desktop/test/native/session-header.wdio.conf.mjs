import { fileURLToPath } from "node:url";
import { config as native } from "./native-terminal.wdio.conf.mjs";

export const config = {
  ...native,
  specs: [fileURLToPath(new URL("session-header.e2e.mjs", import.meta.url))],
};
