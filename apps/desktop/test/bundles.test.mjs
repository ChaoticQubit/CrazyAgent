import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const config = (name) => JSON.parse(readFileSync(new URL(`../src-tauri/${name}`, import.meta.url), "utf8"));
const SIGNING = /certificate|thumbprint|signingIdentity|providerShortName|privateKey|password|secret|token|timestampUrl|apple|azure/i;

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

test("Windows binaries are signed only through the release signing script", () => {
  const windows = config("tauri.windows.conf.json");
  assert.deepEqual(windows.bundle.windows.signCommand, { cmd: "node", args: ["../../../scripts/release/sign-windows.mjs", "%1"] });
});

test("macOS bundles build the app, a disk image and updater archives under the hardened runtime", () => {
  const main = config("tauri.conf.json");
  assert.equal(main.bundle.active, true);
  assert.deepEqual(main.bundle.targets, ["app", "dmg"]);
  assert.equal(main.bundle.createUpdaterArtifacts, true);
  assert.deepEqual(main.bundle.macOS, { hardenedRuntime: true, entitlements: "Entitlements.plist" });
  const entitlements = readFileSync(new URL("../src-tauri/Entitlements.plist", import.meta.url), "utf8");
  assert.deepEqual([...entitlements.matchAll(/<key>([^<]+)<\/key>/g)].map((match) => match[1]), ["com.apple.security.cs.allow-jit"]);
});

test("platform bundles keep the Ember 4 icons from the main config, carry no signing secrets and no sidecar list", () => {
  const main = config("tauri.conf.json");
  assert.ok(main.bundle.icon.includes("icons/icon.png"));
  assert.ok(main.bundle.icon.includes("icons/icon.ico"));
  for (const name of ["tauri.conf.json", "tauri.linux.conf.json", "tauri.windows.conf.json"]) {
    const platform = config(name);
    assert.deepEqual(platform.bundle.icon ?? main.bundle.icon, main.bundle.icon, name);
    const { pubkey: _updaterKey, ...updater } = platform.plugins?.updater ?? {};
    const signing = keys({ ...platform, plugins: { ...platform.plugins, updater } }).filter((key) => SIGNING.test(key));
    assert.deepEqual(signing, [], name);
    assert.equal(platform.bundle.externalBin, undefined, name);
  }
});

test("the updater config holds only the release public key, never a secret key", () => {
  const { pubkey } = config("tauri.conf.json").plugins.updater;
  if (pubkey === "REPLACE_WITH_LEADER_PUBKEY") return;
  const text = Buffer.from(pubkey, "base64").toString("utf8");
  assert.match(text, /^untrusted comment: minisign public key/);
  assert.doesNotMatch(text, /secret key/i);
  assert.equal(text.trim().split("\n").length, 2);
});
