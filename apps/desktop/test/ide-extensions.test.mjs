import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const confirm = await import("../src/screens/project/ide-extensions.tsx");

test("the confirm names an install, an update and the newest version plainly", () => {
  assert.deepEqual(confirm.askWords({ id: "acme.tool", version: "1.2.0", current: null }), {
    title: "Install acme.tool 1.2.0?",
    confirm: "Install",
  });
  assert.deepEqual(confirm.askWords({ id: "acme.tool", version: "1.3.0", current: "1.2.0" }), {
    title: "Update acme.tool from 1.2.0 to 1.3.0?",
    confirm: "Update",
  });
  assert.equal(confirm.askWords({ id: "acme.tool", version: "latest", current: null }).title, "Install acme.tool the newest version?");
});

test("a refused approval says why, and an unknown code is named", () => {
  assert.equal(
    confirm.extensionRefusal({ code: "ide_extension_hash_mismatch" }),
    "The download didn't match the checksum Open VSX lists, so it wasn't approved.",
  );
  assert.equal(
    confirm.extensionRefusal({ code: "ide_extension_ai_locked" }),
    "CrazyAgent keeps AI features out of the IDE, so this extension can't be installed.",
  );
  assert.match(confirm.extensionRefusal({ code: "ide_extension_download_failed" }), /ide_extension_download_failed/);
});

test("every extension command the confirm calls is a registered, allowed desktop command", () => {
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const build = readFileSync(new URL("../src-tauri/build.rs", import.meta.url), "utf8");
  const allowed = readFileSync(new URL("../src-tauri/capabilities/main-window.json", import.meta.url), "utf8");
  for (const command of ["desktop_ide_extension_asks", "desktop_ide_extension_approve", "desktop_ide_extension_decline"]) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`));
    assert.match(lib, new RegExp(`^\\s+${command},$`, "m"));
    assert.match(build, new RegExp(`"${command}"`));
    assert.match(allowed, new RegExp(`"allow-${command.replaceAll("_", "-")}"`));
  }
});
