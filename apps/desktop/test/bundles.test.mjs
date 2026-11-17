import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

const config = (name) => JSON.parse(readFileSync(new URL(`../src-tauri/${name}`, import.meta.url), "utf8"));
const packaged = (path) => new URL(`../src-tauri/${path}`, import.meta.url);
const packagedText = (path) => (existsSync(packaged(path)) ? readFileSync(packaged(path), "utf8") : "");
const STAGED_PROFILE = "/usr/share/crazyagent/apparmor/crazyagent-landlock";
const PROFILE_SOURCE = "linux/apparmor/crazyagent-landlock";
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

test("Linux bundles build a deb and an AppImage that carry their own tmux, so the deb depends on no distro tmux, and the deb brings bubblewrap so agents can run nested (DEC-157, DEC-179 T5)", () => {
  const linux = config("tauri.linux.conf.json");
  assert.equal(linux.bundle.active, true);
  assert.deepEqual(linux.bundle.targets, ["deb", "appimage"]);
  assert.deepEqual(linux.bundle.linux.deb.depends, ["xdg-utils", "bubblewrap"]);
  assert.equal(linux.bundle.linux.appimage.bundleMediaFramework, false);
});

test("the deb stages the helper's AppArmor profile outside /etc/apparmor.d and brings the scripts that install and remove it (DEC-179 SBX-3)", () => {
  const { deb } = config("tauri.linux.conf.json").bundle.linux;
  assert.deepEqual(deb.files, { [STAGED_PROFILE]: PROFILE_SOURCE });
  assert.ok(!Object.keys(deb.files ?? {}).some((path) => path.startsWith("/etc/")), "AppArmor loads every file in /etc/apparmor.d at boot, so the deb ships none there");
  assert.equal(deb.postInstallScript, "linux/deb/postinst");
  assert.equal(deb.preRemoveScript, "linux/deb/prerm");
  for (const path of [PROFILE_SOURCE, deb.postInstallScript, deb.preRemoveScript]) {
    assert.ok(path && existsSync(packaged(path)), path);
  }
});

test("the AppArmor profile attaches only to the helper, keeps the bubblewrap it starts in its own profile, holds every other program to no capability and lets only a CrazyAgent process trace the helper (DEC-179 SBX-3, rv-ds241 F1)", () => {
  const profile = packagedText(PROFILE_SOURCE);
  assert.match(profile, /^abi <abi\/4\.0>,$/m, "the AppArmor 4.0 that Ubuntu 24.04 ships");
  const headers = [...profile.matchAll(/^profile (\S+)(?: (\/\S+))? flags=\(attach_disconnected,mediate_deleted\) \{$/gm)];
  assert.deepEqual(headers.map(([, name, path]) => [name, path]), [["crazyagent-landlock", "/usr/bin/crazyagent-landlock"], ["crazyagent-agent", undefined]]);
  assert.doesNotMatch(profile, /\/usr\/bin\/bwrap (?:flags|\{)/, "no profile attaches to the system bubblewrap");
  const block = (name) => profile.split(/^profile /m).find((part) => part.startsWith(`${name} `)) ?? "";
  const helper = block("crazyagent-landlock");
  const agent = block("crazyagent-agent");
  for (const rule of ["allow capability,", "allow userns,", "allow mount,", "allow pivot_root,", "allow /usr/bin/bwrap ix,", "allow px /** -> crazyagent-landlock//&crazyagent-agent,"]) {
    assert.ok(helper.split("\n").map((line) => line.trim()).includes(rule), `helper: ${rule}`);
  }
  for (const rule of ["audit deny capability,", "allow pix /** -> &crazyagent-agent,"]) {
    assert.ok(agent.split("\n").map((line) => line.trim()).includes(rule), `agent: ${rule}`);
  }
  assert.ok(!agent.split("\n").some((line) => line.trim() === "allow capability,"), "agents hold no capability");
  const traced = helper.split("\n").map((line) => line.trim()).filter((line) => /\bptrace\b/.test(line));
  assert.deepEqual(traced, ["allow ptrace (read, trace, readby),", "allow ptrace (tracedby) peer=crazyagent-{landlock,agent},"], "a tracer outside CrazyAgent's two profiles is refused");
  assert.doesNotMatch(profile, /\b[pPcC]?[uU]x\b/, "nothing runs unconfined");
  assert.deepEqual(profile.split("\n").filter((line) => line.trimStart().startsWith("#")), [], "no comment lines");
});

test("postinst installs and loads the profile only on configure where AppArmor runs, restricts user namespaces, parses the file and no admin disabled it, and never fails the install; prerm unloads and deletes it on remove (DEC-179 SBX-3)", () => {
  const postinst = packagedText("linux/deb/postinst");
  const prerm = packagedText("linux/deb/prerm");
  for (const [name, script] of [["postinst", postinst], ["prerm", prerm]]) {
    assert.equal(script.split("\n")[0], "#!/bin/sh", name);
    assert.deepEqual(script.split("\n").filter((line) => line.trimStart().startsWith("#")), ["#!/bin/sh"], `${name}: no comment lines`);
    assert.match(script, /^set -e$/m, name);
    assert.match(script, /^dst=\/etc\/apparmor\.d\/crazyagent-landlock$/m, name);
  }
  assert.match(postinst, new RegExp(`^src=${STAGED_PROFILE.replaceAll("/", "\\/")}$`, "m"));
  for (const guard of ['[ "$1" = configure ]', "[ -d /sys/kernel/security/apparmor ]", "[ ! -e /etc/apparmor.d/disable/crazyagent-landlock ]", "[ ! -L /etc/apparmor.d/disable/crazyagent-landlock ]", "command -v apparmor_parser", '"$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1', 'apparmor_parser -QT "$src"']) {
    assert.ok(postinst.includes(guard), guard);
  }
  const steps = ['apparmor_parser -QT "$src"', 'install -m 0644 "$src" "$dst"', 'apparmor_parser -r -T -W "$dst" || true'].map((step) => postinst.indexOf(step));
  assert.ok(steps.every((at) => at >= 0) && steps[0] < steps[1] && steps[1] < steps[2], "checked, then copied, then loaded without failing the install");
  assert.ok(postinst.includes('if install -m 0644 "$src" "$dst"; then'), "a copy that fails never fails the install");
  assert.ok(postinst.includes(`{ [ -f "$dst" ] || [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; }`), "an upgrade refreshes a copy already there, wherever the restriction stands now");
  assert.ok(prerm.includes('[ "$1" = remove ]'), "an upgrade keeps the profile loaded");
  assert.ok(prerm.includes('apparmor_parser -R "$dst" || true'));
  assert.ok(prerm.includes('rm -f "$dst" || true'), "a copy that cannot be deleted never fails the removal");
  assert.ok(!prerm.includes("/usr/share/crazyagent"), "dpkg removes the staged copy itself");
});

test("the engine's AppArmor line names the deb's own package for dpkg-reconfigure (DEC-179 SBX-3)", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox.rs", import.meta.url), "utf8");
  const line = /pub const LINUX_APPARMOR_LINE: &str = "([^"]+)";/.exec(engine)?.[1] ?? "";
  const kebab = config("tauri.conf.json").productName.replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase();
  assert.equal(kebab, "crazy-agent", "Tauri names the deb package in kebab case");
  assert.ok(line.includes(`sudo dpkg-reconfigure ${kebab}`), line);
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

test("the deb recommends slirp4netns and the nf_tables iptables, so where apt installs recommends a Port 443 only agent gets a network of its own, and a host without them still installs the app (DEC-179 SBX-2)", () => {
  const { deb } = config("tauri.linux.conf.json").bundle.linux;
  assert.deepEqual(deb.recommends, ["slirp4netns", "iptables"]);
  assert.deepEqual(deb.depends, ["xdg-utils", "bubblewrap"], "neither is a hard dependency");
});

test("the helper runs itself, slirp4netns and the nf_tables iptables inside its own profile, so its setup child keeps the two network capabilities it raises and no agent gets them (DEC-179 SBX-2)", () => {
  const profile = packagedText(PROFILE_SOURCE);
  const block = (name) => profile.split(/^profile /m).find((part) => part.startsWith(`${name} `)) ?? "";
  const lines = (name) => block(name).split("\n").map((line) => line.trim());
  for (const rule of ["allow /usr/bin/crazyagent-landlock ix,", "allow /usr/bin/slirp4netns ix,", "allow /usr/sbin/xtables-nft-multi ix,"]) {
    assert.ok(lines("crazyagent-landlock").includes(rule), `helper: ${rule}`);
    assert.ok(!lines("crazyagent-agent").includes(rule), `agent: ${rule}`);
  }
});
