import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { NOTES_MAX, baseVersion, changelogSection } from "./notes.mjs";

const NOTES = fileURLToPath(new URL("./notes.mjs", import.meta.url));
const CHANGELOG = fileURLToPath(new URL("../../CHANGELOG.md", import.meta.url));

const SAMPLE = [
  "# Changelog",
  "",
  "Status key: Released, Dev build, Internal build, In progress.",
  "",
  "## [Unreleased]",
  "",
  "## [2.4.10] - 2026-12-01 - Dev build",
  "",
  "### Fixed",
  "",
  "- Ten.",
  "",
  "## [2.4.1] - 2026-11-14 02:36 UTC - In progress (not on master)",
  "",
  "- Not built yet.",
  "",
  "### Added",
  "",
  "- Bundled tmux.",
  "",
  "## [2.4.0] - 2026-11-13 19:41 UTC - Dev build",
  "",
  "- Dev build 2.4.0-dev.202611131902.",
  "",
  "### Added",
  "",
  "- Learn CrazyAgent.",
  "#### Not a version heading",
  "- Costs `$5` and keeps 'quotes'.",
  "",
  "## [2.3.2] - 2026-11-12 16:18 UTC - Released",
  "",
  "### Fixed",
  "",
  "- Short team view.",
  "",
  "## [V2] - 2026-11-09 06:20 UTC - Internal build",
  "",
  "- Internal.",
  "",
].join("\n");

test("a dev build version reads its base version, and a stable version is its own base", () => {
  assert.equal(baseVersion("2.4.0-dev.202611131902"), "2.4.0");
  assert.equal(baseVersion("2.4.0"), "2.4.0");
  assert.equal(baseVersion("12.0.7-dev.202701020304"), "12.0.7");
  for (const bad of ["2.4", "v2.4.0", "2.4.0-rc.1", "2.4.0-dev.", "2.4.0\n", "", undefined]) {
    assert.throws(() => baseVersion(bad), /release_notes_version_rejected/, String(bad));
  }
});

test("the section of a version is every line under its heading up to the next version heading, without the heading", () => {
  assert.equal(changelogSection(SAMPLE, "2.4.0"), [
    "- Dev build 2.4.0-dev.202611131902.",
    "",
    "### Added",
    "",
    "- Learn CrazyAgent.",
    "#### Not a version heading",
    "- Costs `$5` and keeps 'quotes'.",
  ].join("\n"));
  assert.equal(changelogSection(SAMPLE, "2.3.2"), "### Fixed\n\n- Short team view.");
  assert.equal(changelogSection(SAMPLE, "2.4.1"), "- Not built yet.\n\n### Added\n\n- Bundled tmux.");
});

test("a dev build gets its base version's section", () => {
  assert.equal(changelogSection(SAMPLE, "2.4.0-dev.202611131902"), changelogSection(SAMPLE, "2.4.0"));
  assert.equal(changelogSection(SAMPLE, "2.4.1-dev.202611140300"), changelogSection(SAMPLE, "2.4.1"));
});

test("a version matches only its own heading, never a longer version or a dotted look-alike", () => {
  assert.equal(changelogSection(SAMPLE, "2.4.10"), "### Fixed\n\n- Ten.");
  assert.equal(changelogSection(SAMPLE, "2.4.1"), "- Not built yet.\n\n### Added\n\n- Bundled tmux.");
  assert.equal(changelogSection(SAMPLE.replace("## [2.3.2]", "## [2x3x2]"), "2.3.2"), "");
});

test("no section, an empty section or no changelog gives no text", () => {
  assert.equal(changelogSection(SAMPLE, "9.9.9"), "");
  assert.equal(changelogSection("## [1.0.0] - 2026-01-01 - Released\n\n## [0.9.0] - 2025-12-01 - Released\n- old\n", "1.0.0"), "");
  assert.equal(changelogSection("", "2.4.0"), "");
});

test("a section past the release notes bound is cut at a line and says where the rest is", () => {
  const long = Array.from({ length: 20_000 }, (_, index) => `- Line ${index} of a very long section.`).join("\n");
  const text = changelogSection(`## [3.0.0] - 2027-01-01 - Released\n\n${long}\n`, "3.0.0");
  assert(text.length <= NOTES_MAX, `${text.length} <= ${NOTES_MAX}`);
  assert(text.endsWith("\n\nThe rest of this version's notes is in CHANGELOG.md."), text.slice(-80));
  assert(text.startsWith("- Line 0 of a very long section.\n- Line 1"));
});

test("the command prints the section, and prints nothing for a missing changelog or version", () => {
  const root = mkdtempSync(join(tmpdir(), "release-notes-"));
  try {
    const file = join(root, "CHANGELOG.md");
    writeFileSync(file, SAMPLE);
    const run = (path, version) => execFileSync(process.execPath, [NOTES, path, version], { encoding: "utf8" });
    assert.equal(run(file, "2.3.2-dev.202611121542"), "### Fixed\n\n- Short team view.\n");
    assert.equal(run(file, "9.9.9"), "");
    assert.equal(run(join(root, "missing.md"), "2.3.2"), "");
    assert.throws(() => run(file, "not-a-version"), /release_notes_version_rejected/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the repository changelog has a non-empty section for each released and dev-built version", () => {
  const text = readFileSync(CHANGELOG, "utf8");
  for (const version of ["2.4.0", "2.3.2", "2.2.0"]) {
    const section = changelogSection(text, version);
    assert.match(section, /^### (Added|Changed|Fixed|Security)$/m, version);
    assert(!/^## /m.test(section), `${version} stops at the next version`);
    assert(section.length < NOTES_MAX, `${version} fits the release notes bound`);
  }
});
