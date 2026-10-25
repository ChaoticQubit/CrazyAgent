import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { TARGETS, cliArchiveName, desktopLayout, signFile } from "./assets.mjs";
import { writeManifests } from "./manifest.mjs";

const REPO = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const TAURI = join(REPO, "apps/desktop/node_modules/@tauri-apps/cli/tauri.js");
const VERSION = "0.2.0-dev.202610251607";
const COMMIT = "5254fb1d0c1e7a1b2c3d4e5f60718293a4b5c6d7";
const BASE = "https://github.com/ChaoticQubit/CrazyAgent/releases/download/dev";
const PUB_DATE = "2026-10-25T16:07:00Z";

async function keyPair(root) {
  const path = join(root, "updater.key");
  const password = `throwaway-${process.pid}`;
  execFileSync(process.execPath, [TAURI, "signer", "generate", "--ci", "-w", path, "-p", password], { stdio: "ignore" });
  const env = { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PATH: path, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password };
  delete env.TAURI_SIGNING_PRIVATE_KEY;
  return { env, publicKey: (await readFile(`${path}.pub`, "utf8")).trim() };
}

async function releaseDir(root, env, targets = Object.keys(TARGETS)) {
  const dir = join(root, "assets");
  await mkdir(dir);
  for (const target of targets) {
    const names = [...desktopLayout(target, VERSION).map((item) => item.to), cliArchiveName(VERSION, target)];
    for (const name of names) {
      await writeFile(join(dir, name), `${target} ${name}\n`);
      await signFile({ file: join(dir, name), version: VERSION, env });
    }
  }
  return dir;
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

test("a full release gets both update manifests and checksums over every asset", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-manifest-"));
  try {
    const { env, publicKey } = await keyPair(root);
    const dir = await releaseDir(root, env);
    const sig = async (name) => (await readFile(join(dir, `${name}.sig`), "utf8")).trim();
    const written = await writeManifests({ dir, version: VERSION, channel: "dev", commit: COMMIT, baseUrl: BASE, notes: "Dev build 5254fb1", pubDate: PUB_DATE, publicKey });
    assert.deepEqual(written, ["latest.json", "cli-latest.json", "SHA256SUMS"].map((name) => join(dir, name)));
    const latest = JSON.parse(await readFile(join(dir, "latest.json"), "utf8"));
    const entry = async (name) => ({ signature: await sig(name), url: `${BASE}/${name}` });
    assert.deepEqual(latest, {
      version: VERSION,
      notes: "Dev build 5254fb1",
      pub_date: PUB_DATE,
      platforms: {
        "darwin-aarch64": await entry(`CrazyAgent_${VERSION}_aarch64.app.tar.gz`),
        "darwin-aarch64-app": await entry(`CrazyAgent_${VERSION}_aarch64.app.tar.gz`),
        "darwin-x86_64": await entry(`CrazyAgent_${VERSION}_x64.app.tar.gz`),
        "darwin-x86_64-app": await entry(`CrazyAgent_${VERSION}_x64.app.tar.gz`),
        "linux-x86_64": await entry(`CrazyAgent_${VERSION}_amd64.AppImage`),
        "linux-x86_64-appimage": await entry(`CrazyAgent_${VERSION}_amd64.AppImage`),
        "linux-x86_64-deb": await entry(`CrazyAgent_${VERSION}_amd64.deb`),
        "windows-x86_64": await entry(`CrazyAgent_${VERSION}_x64-setup.exe`),
        "windows-x86_64-nsis": await entry(`CrazyAgent_${VERSION}_x64-setup.exe`),
      },
    });
    const cli = JSON.parse(await readFile(join(dir, "cli-latest.json"), "utf8"));
    const platforms = {};
    for (const target of Object.keys(TARGETS)) {
      const name = cliArchiveName(VERSION, target);
      platforms[target] = { url: `${BASE}/${name}`, sha256: sha256(`${target} ${name}\n`), signature: await sig(name) };
    }
    assert.deepEqual(cli, { version: VERSION, channel: "dev", commit: COMMIT, pub_date: PUB_DATE, platforms });
    const files = (await readdir(dir)).filter((name) => name !== "SHA256SUMS").sort();
    const expected = await Promise.all(files.map(async (name) => `${sha256(await readFile(join(dir, name)))}  ${name}\n`));
    assert.equal(await readFile(join(dir, "SHA256SUMS"), "utf8"), expected.join(""));
    assert.equal(files.length, (7 + 4) * 2 + 2);
    execFileSync("shasum", ["-a", "256", "-c", "SHA256SUMS"], { cwd: dir, stdio: "ignore" });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a release with a missing, foreign, unexpected or wrongly signed asset is refused", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-manifest-bad-"));
  try {
    const { env, publicKey } = await keyPair(root);
    const other = await keyPair(await mkdtemp(join(tmpdir(), "release-manifest-other-")));
    const dir = await releaseDir(root, env, ["x86_64-pc-windows-msvc"]);
    const input = { dir, version: VERSION, channel: "dev", commit: COMMIT, baseUrl: BASE, notes: "", pubDate: PUB_DATE, publicKey, targets: ["x86_64-pc-windows-msvc"] };
    await assert.rejects(writeManifests({ ...input, targets: undefined }), /release_asset_missing:CrazyAgent_0\.2\.0-dev\.202610251607_aarch64\.app\.tar\.gz/);
    await assert.rejects(writeManifests({ ...input, publicKey: other.publicKey }), /release_signature_key_mismatch/);
    await assert.rejects(writeManifests({ ...input, version: "0.2.0-dev.202610251608" }), /release_asset_missing/);
    await assert.rejects(writeManifests({ ...input, version: "0.2.0" }), /release_channel_version_rejected/);
    await assert.rejects(writeManifests({ ...input, channel: "stable" }), /release_channel_version_rejected/);
    await assert.rejects(writeManifests({ ...input, commit: "5254fb1" }), /release_commit_rejected/);
    await assert.rejects(writeManifests({ ...input, baseUrl: "http://example.test/dev" }), /release_base_url_rejected/);
    await writeFile(join(dir, "release.conf.json"), "{}\n");
    await assert.rejects(writeManifests(input), /release_asset_unexpected:release\.conf\.json/);
    await rm(join(dir, "release.conf.json"));
    await rm(join(dir, `CrazyAgent_${VERSION}_x64-setup.exe.sig`));
    await assert.rejects(writeManifests(input), /release_asset_missing:CrazyAgent_0\.2\.0-dev\.202610251607_x64-setup\.exe\.sig/);
    await signFile({ file: join(dir, `CrazyAgent_${VERSION}_x64-setup.exe`), version: "0.1.9", env });
    await assert.rejects(writeManifests(input), /release_signature_version_mismatch/);
    assert.deepEqual((await readdir(dir)).filter((name) => /json$|SUMS$/.test(name)), []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a stable release takes a plain version and the stable download base", async () => {
  const root = await mkdtemp(join(tmpdir(), "release-manifest-stable-"));
  try {
    const { env, publicKey } = await keyPair(root);
    const dir = join(root, "assets");
    await mkdir(dir);
    const target = "x86_64-unknown-linux-gnu";
    for (const name of [...desktopLayout(target, "0.2.0").map((item) => item.to), cliArchiveName("0.2.0", target)]) {
      await writeFile(join(dir, name), `${name}\n`);
      await signFile({ file: join(dir, name), version: "0.2.0", env });
    }
    const base = "https://github.com/ChaoticQubit/CrazyAgent/releases/download/v0.2.0";
    await writeManifests({ dir, version: "0.2.0", channel: "stable", commit: COMMIT, baseUrl: base, notes: "CrazyAgent 0.2.0", pubDate: PUB_DATE, publicKey, targets: [target] });
    const cli = JSON.parse(await readFile(join(dir, "cli-latest.json"), "utf8"));
    assert.equal(cli.channel, "stable");
    assert.equal(cli.platforms[target].url, `${base}/crazyagent-0.2.0-${target}.tar.gz`);
    assert.deepEqual(Object.keys(JSON.parse(await readFile(join(dir, "latest.json"), "utf8")).platforms), ["linux-x86_64", "linux-x86_64-appimage", "linux-x86_64-deb"]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
