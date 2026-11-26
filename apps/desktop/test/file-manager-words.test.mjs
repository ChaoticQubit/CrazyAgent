import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("the show-folder actions name Finder only on macOS and say Open folder on Windows and Linux", () => {
  for (const file of ["../src/screens/project/context.tsx", "../src/screens/project/index.tsx"]) {
    const lines = readFileSync(new URL(file, import.meta.url), "utf8").split("\n").filter((line) => line.includes("in Finder"));
    assert.ok(lines.length > 0, file);
    for (const line of lines) assert.match(line, /isMac\(\) \? "Show (folder )?in Finder" : "Open folder"/, `${file}: ${line.trim()}`);
  }
});
