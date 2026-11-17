import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const RELEASE = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");
const PROBE = "Check this runner reads the prebuilt app icon";

/**
 * Returns where the release workflow's one step named `name` starts, at its `- name:` line.
 * Input: the step name. Output: the offset in release.yml; it fails unless exactly one step has the name.
 */
function stepAt(name) {
  const marker = `\n      - name: ${name}\n`;
  assert.equal(RELEASE.split(marker).length, 2, `one release step is named ${name}`);
  return RELEASE.indexOf(marker) + 1;
}

test("on macOS the release job checks in its first minute that the runner's assetutil reads the prebuilt Assets.car, its Icon Image named Icon and its dark look, before the long compile (DEC-181)", () => {
  const job = RELEASE.indexOf("\n  build:\n");
  const probe = stepAt(PROBE);
  assert(job >= 0 && job < RELEASE.indexOf("actions/checkout@", job) && RELEASE.indexOf("actions/checkout@", job) < probe, "the car it reads is checked out first");
  assert(probe < stepAt("Build command line binaries"), "a runner that cannot read the car fails before the long compile");
  const next = RELEASE.indexOf("\n      - ", probe);
  const step = RELEASE.slice(probe, next);
  assert.match(step, /^ {8}if: matrix\.os == 'macos'$/m);
  assert.match(step, /^ {10}car=apps\/desktop\/src-tauri\/icons\/Assets\.car$/m);
  assert.match(step, /info="\$\(assetutil --info "\$car"\)" \|\| \{ echo "::error::[^"]*"; exit 1; \}/);
  for (const check of ['"AssetType"\\[\\[:space:\\]\\]\\*:\\[\\[:space:\\]\\]\\*"Icon Image"', '"Name"\\[\\[:space:\\]\\]\\*:\\[\\[:space:\\]\\]\\*"Icon"', '"NSAppearanceNameDarkAqua"']) {
    assert.match(step, new RegExp(`grep -E?q '${check}' <<< "\\$info" \\|\\| \\{ echo "::error::[^"]*DEC-181\\)"; exit 1; \\}`), check);
  }
});
