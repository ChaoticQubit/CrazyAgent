import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const config = (name) => JSON.parse(readFileSync(new URL(`../src-tauri/${name}`, import.meta.url), "utf8"));
const SIGNING = /certificate|thumbprint|signCommand|signingIdentity|pubkey|privateKey|password|secret|token|timestampUrl/i;

function keys(value, found = []) {
  if (value && typeof value === "object") {
    for (const [key, inner] of Object.entries(value)) {
      found.push(key);
      keys(inner, found);
    }
  }
  return found;
}

test("Linux bundles build a deb and an AppImage that pull in tmux", () => {
  const linux = config("tauri.linux.conf.json");
  assert.equal(linux.bundle.active, true);
  assert.deepEqual(linux.bundle.targets, ["deb", "appimage"]);
  assert.deepEqual(linux.bundle.linux.deb.depends, ["tmux", "xdg-utils"]);
  assert.equal(linux.bundle.linux.appimage.bundleMediaFramework, false);
});

test("Windows bundles build a per-user NSIS installer with no administrator rights", () => {
  const windows = config("tauri.windows.conf.json");
  assert.equal(windows.bundle.active, true);
  assert.deepEqual(windows.bundle.targets, ["nsis"]);
  assert.equal(windows.bundle.windows.nsis.installMode, "currentUser");
});

test("platform bundles keep the Ember 4 icons from the main config and carry no signing settings", () => {
  const main = config("tauri.conf.json");
  assert.equal(main.bundle.active, false, "macOS bundling stays as it was");
  assert.ok(main.bundle.icon.includes("icons/icon.png"));
  assert.ok(main.bundle.icon.includes("icons/icon.ico"));
  for (const name of ["tauri.conf.json", "tauri.linux.conf.json", "tauri.windows.conf.json"]) {
    const platform = config(name);
    assert.deepEqual(platform.bundle.icon ?? main.bundle.icon, main.bundle.icon, name);
    const signing = keys(platform).filter((key) => SIGNING.test(key));
    assert.deepEqual(signing, [], name);
  }
});
