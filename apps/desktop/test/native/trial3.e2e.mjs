import assert from "node:assert/strict";
import { MODE, step } from "./trial-kit.mjs";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";

assert(["live", "fixture"].includes(MODE), "TRIAL3_MODE is live or fixture");

describe(`CrazyAgent trial 3 owner actions on the desktop app (${MODE})`, () => {
  for (const { slug, it: name, title, why, action } of TRIAL3_STEPS) it(name, () => step(slug, title, why, action));
});
