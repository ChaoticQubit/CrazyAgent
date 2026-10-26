import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { cliArchiveName, collectDesktop, packageCli, sidecars, signFile, stageSidecars, verifySignature } from "./assets.mjs";

const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const TAURI = join(REPO, "apps/desktop/node_modules/@tauri-apps/cli/tauri.js");
const VERSION = "0.2.0-dev.202610251607";

async function scratch(prefix) {
  return mkdtemp(join(tmpdir(), prefix));
}

async function keyPair(root) {
  const path = join(root, "updater.key");
  const password = `throwaway-${process.pid}`;
  execFileSync(process.execPath, [TAURI, "signer", "generate", "--ci", "-w", path, "-p", password], { stdio: "ignore" });
  const env = { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PATH: path, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password };
  delete env.TAURI_SIGNING_PRIVATE_KEY;
  return { env, publicKey: (await readFile(`${path}.pub`, "utf8")).trim() };
}

async function executable(path, text) {
  await writeFile(path, `#!/bin/sh\necho ${text}\n`);
  await chmod(path, 0o755);
}

test("each target ships the engine, the rcli host and its own helpers", () => {
  assert.deepEqual(sidecars("aarch64-apple-darwin"), ["crazyagent", "crazyagent-rcli", "crazyagent-runner"]);
  assert.deepEqual(sidecars("x86_64-apple-darwin"), ["crazyagent", "crazyagent-rcli", "crazyagent-runner"]);
  assert.deepEqual(sidecars("x86_64-unknown-linux-gnu"), ["crazyagent", "crazyagent-rcli", "crazyagent-runner", "crazyagent-landlock"]);
  assert.deepEqual(sidecars("x86_64-pc-windows-msvc"), ["crazyagent", "crazyagent-rcli", "crazyagent-runner", "crazyagent-win-boundary"]);
  assert.throws(() => sidecars("aarch64-unknown-linux-gnu"), /release_target_rejected/);
  assert.equal(cliArchiveName(VERSION, "x86_64-unknown-linux-gnu"), `crazyagent-${VERSION}-x86_64-unknown-linux-gnu.tar.gz`);
  assert.equal(cliArchiveName(VERSION, "x86_64-pc-windows-msvc"), `crazyagent-${VERSION}-x86_64-pc-windows-msvc.zip`);
});

test("staging names sidecars with the target triple and writes the release config overlay", async () => {
  const root = await scratch("release-stage-");
  try {
    const cargo = join(root, "cargo");
    const tauri = join(root, "src-tauri");
    await mkdir(cargo);
    await mkdir(tauri);
    for (const name of ["crazyagent.exe", "crazyagent-runner.exe", "crazyagent-win-boundary.exe"]) await executable(join(cargo, name), name);
    await executable(join(root, "crazyagent-rcli.exe"), "rcli");
    const target = "x86_64-pc-windows-msvc";
    const { staged, config } = await stageSidecars({ target, cargoDir: cargo, rcli: join(root, "crazyagent-rcli.exe"), tauriDir: tauri });
    assert.deepEqual(staged.map((path) => path.slice(tauri.length + 1)), sidecars(target).map((name) => `binaries/${name}-${target}.exe`));
    assert.equal(await readFile(join(tauri, `binaries/crazyagent-rcli-${target}.exe`), "utf8"), "#!/bin/sh\necho rcli\n");
    for (const path of staged) assert.equal((await stat(path)).mode & 0o777, 0o755);
    assert.equal(config, join(tauri, "binaries/release.conf.json"));
    assert.deepEqual(JSON.parse(await readFile(config, "utf8")), { bundle: { externalBin: sidecars(target).map((name) => `binaries/${name}`) } });
    await rm(join(cargo, "crazyagent-win-boundary.exe"));
    await assert.rejects(stageSidecars({ target, cargoDir: cargo, rcli: join(root, "crazyagent-rcli.exe"), tauriDir: tauri }), /release_sidecar_missing:crazyagent-win-boundary/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("CLI archives hold exactly the target's executables, signed and bound to the version", async () => {
  const root = await scratch("release-cli-");
  try {
    const { env, publicKey } = await keyPair(root);
    for (const target of ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]) {
      const exe = target.includes("windows") ? ".exe" : "";
      const source = join(root, `source-${target}`);
      const out = join(root, `out-${target}`);
      await mkdir(source);
      await mkdir(out);
      for (const name of sidecars(target)) await executable(join(source, `${name}-${target}${exe}`), name);
      const archive = await packageCli({ target, version: VERSION, source, suffixed: true, out });
      assert.equal(archive, join(out, cliArchiveName(VERSION, target)));
      const listed = execFileSync("tar", ["-tf", archive]).toString().trim().split("\n").sort();
      assert.deepEqual(listed, sidecars(target).map((name) => `${name}${exe}`).sort());
      const unpacked = join(root, `unpacked-${target}`);
      await mkdir(unpacked);
      execFileSync("tar", ["-xf", archive, "-C", unpacked]);
      for (const name of sidecars(target)) {
        assert.equal(await readFile(join(unpacked, `${name}${exe}`), "utf8"), `#!/bin/sh\necho ${name}\n`);
        assert.equal((await stat(join(unpacked, `${name}${exe}`))).mode & 0o111, 0o111);
      }
      const signature = await signFile({ file: archive, version: VERSION, env });
      assert.equal(signature, `${archive}.sig`);
      assert.deepEqual(await verifySignature({ file: archive, signature: await readFile(signature, "utf8"), publicKey, version: VERSION }), { version: VERSION, file: cliArchiveName(VERSION, target) });
      await assert.rejects(packageCli({ target, version: VERSION, source, suffixed: true, out }), /release_asset_exists/);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a signature fails for changed bytes, another version, another key or a missing key", async () => {
  const root = await scratch("release-sig-");
  try {
    const { env, publicKey } = await keyPair(root);
    const other = await keyPair(await scratch("release-sig-other-"));
    const file = join(root, "asset.bin");
    await writeFile(file, "release asset bytes\n");
    const signature = await readFile(await signFile({ file, version: VERSION, env }), "utf8");
    assert.equal((await verifySignature({ file, signature, publicKey, version: VERSION })).version, VERSION);
    await assert.rejects(verifySignature({ file, signature, publicKey, version: "0.2.0" }), /release_signature_version_mismatch/);
    await assert.rejects(verifySignature({ file, signature, publicKey: other.publicKey, version: VERSION }), /release_signature_key_mismatch/);
    const forged = Buffer.from(Buffer.from(signature, "base64").toString("utf8").replace(`version:${VERSION}`, "version:9.9.9")).toString("base64");
    assert.notEqual(forged, signature);
    await assert.rejects(verifySignature({ file, signature: forged, publicKey, version: "9.9.9" }), /release_signature_invalid/);
    await writeFile(file, "release asset bytes!\n");
    await assert.rejects(verifySignature({ file, signature, publicKey, version: VERSION }), /release_signature_invalid/);
    const bare = { ...process.env };
    for (const name of ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PATH", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]) delete bare[name];
    await assert.rejects(signFile({ file, version: VERSION, env: bare }), /release_signing_key_missing/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("desktop bundles are collected under release names with a signature for every asset", async () => {
  const root = await scratch("release-collect-");
  try {
    const { env, publicKey } = await keyPair(root);
    const layouts = {
      "aarch64-apple-darwin": [["macos/CrazyAgent.app.tar.gz", `CrazyAgent_${VERSION}_aarch64.app.tar.gz`, true], [`dmg/CrazyAgent_${VERSION}_aarch64.dmg`, `CrazyAgent_${VERSION}_aarch64.dmg`, false]],
      "x86_64-apple-darwin": [["macos/CrazyAgent.app.tar.gz", `CrazyAgent_${VERSION}_x64.app.tar.gz`, true], [`dmg/CrazyAgent_${VERSION}_x64.dmg`, `CrazyAgent_${VERSION}_x64.dmg`, false]],
      "x86_64-unknown-linux-gnu": [[`appimage/CrazyAgent_${VERSION}_amd64.AppImage`, `CrazyAgent_${VERSION}_amd64.AppImage`, true], [`deb/CrazyAgent_${VERSION}_amd64.deb`, `CrazyAgent_${VERSION}_amd64.deb`, true]],
      "x86_64-pc-windows-msvc": [[`nsis/CrazyAgent_${VERSION}_x64-setup.exe`, `CrazyAgent_${VERSION}_x64-setup.exe`, true]],
    };
    for (const [target, layout] of Object.entries(layouts)) {
      const bundle = join(root, `bundle-${target}`);
      const out = join(root, `assets-${target}`);
      await mkdir(out);
      for (const [from, , updater] of layout) {
        await mkdir(join(bundle, from, ".."), { recursive: true });
        await writeFile(join(bundle, from), `${target} ${from}\n`);
        if (updater) await signFile({ file: join(bundle, from), version: VERSION, env });
      }
      const assets = await collectDesktop({ target, version: VERSION, bundleDir: bundle, out, env });
      assert.deepEqual(assets, layout.map(([, to]) => join(out, to)));
      assert.deepEqual((await readdir(out)).sort(), layout.flatMap(([, to]) => [to, `${to}.sig`]).sort());
      for (const [from, to] of layout) {
        assert.equal(await readFile(join(out, to), "utf8"), `${target} ${from}\n`);
        await verifySignature({ file: join(out, to), signature: await readFile(join(out, `${to}.sig`), "utf8"), publicKey, version: VERSION });
      }
    }
    const stale = join(root, "bundle-stale");
    await mkdir(join(stale, "nsis"), { recursive: true });
    await writeFile(join(stale, "nsis/CrazyAgent_0.1.0_x64-setup.exe"), "old\n");
    await assert.rejects(collectDesktop({ target: "x86_64-pc-windows-msvc", version: VERSION, bundleDir: stale, out: await scratch("release-stale-"), env }), /release_bundle_missing:nsis\/CrazyAgent_0\.2\.0-dev\.202610251607_x64-setup\.exe/);
    const unsigned = join(root, "bundle-unsigned");
    await mkdir(join(unsigned, "nsis"), { recursive: true });
    await writeFile(join(unsigned, `nsis/CrazyAgent_${VERSION}_x64-setup.exe`), "unsigned\n");
    await assert.rejects(collectDesktop({ target: "x86_64-pc-windows-msvc", version: VERSION, bundleDir: unsigned, out: await scratch("release-unsigned-"), env }), /release_updater_signature_missing/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
