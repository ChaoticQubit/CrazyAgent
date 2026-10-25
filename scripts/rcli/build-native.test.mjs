import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmod, link, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rename, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { compilerArgv, openPinned, readPinned, validateStaging, planBuild } from "./build-native.mjs";

test("pinned inputs, private staging, declared argv and the real lock keep the native build closed", async () => {
  const root = await mkdtemp(join(await realpath(tmpdir()), "rcli-build-check-"));
  await chmod(root, 0o700);
  const content = Buffer.from("public build input\n");
  const pin = { bytes: content.length, sha256: createHash("sha256").update(content).digest("hex") };
  const source = join(root, "source");
  const stage = join(root, "stage");
  try {
    await writeFile(source, content, { mode: 0o600 });
    await mkdir(stage, { mode: 0o700 });
    assert.deepEqual(await readPinned(await openPinned(source, pin)), content);
    await assert.rejects(openPinned(join(root, "missing"), pin));
    await assert.rejects(openPinned(source, { ...pin, bytes: 0 }), /build_pin_invalid/);
    await assert.rejects(openPinned(source, { ...pin, sha256: "bad" }), /build_pin_invalid/);
    await assert.rejects(readPinned(await openPinned(source, { ...pin, sha256: "0".repeat(64) })), /build_hash_mismatch/);
    await assert.rejects(openPinned(source, { ...pin, bytes: pin.bytes + 1 }), /build_file_unsafe/);
    const alias = join(root, "alias");
    await symlink(source, alias);
    await assert.rejects(openPinned(alias, pin), /build_path_not_canonical/);
    const directoryAlias = join(root, "directory-alias");
    await symlink(root, directoryAlias);
    await assert.rejects(openPinned(join(directoryAlias, "source"), pin), /build_path_not_canonical/);
    const hard = join(root, "hard");
    await link(source, hard);
    await assert.rejects(openPinned(source, pin), /build_file_unsafe/);
    await rm(hard);
    await chmod(source, 0o666);
    await assert.rejects(openPinned(source, pin), /build_file_unsafe/);
    await chmod(source, 0o600);
    const lease = await openPinned(source, pin);
    await rename(source, join(root, "old-source"));
    await writeFile(source, content, { mode: 0o600 });
    await assert.rejects(readPinned(lease), /build_source_replaced/);
    const changing = await openPinned(source, pin);
    await writeFile(source, Buffer.from("changed input bytes"));
    await assert.rejects(readPinned(changing), /build_(file_changed|hash_mismatch)/);
    await writeFile(source, content, { mode: 0o600 });
    assert.equal(await validateStaging(stage), stage);
    await chmod(stage, 0o755);
    await assert.rejects(validateStaging(stage), /build_staging_unsafe/);
    await chmod(stage, 0o700);
    const sentinel = join(stage, "preserve");
    await writeFile(sentinel, content, { mode: 0o600 });
    await assert.rejects(validateStaging(stage), /build_staging_not_empty/);
    assert.deepEqual(await readFile(sentinel), content);
    await rm(sentinel);
    const paths = { bun: "/bound/bun", config: "/bound/bunfig.toml", entry: "/bound/entry.ts", output: "/bound/crazyagent-rcli-host" };
    const argv = compilerArgv(paths, { os: "darwin", arch: "arm64", bun_target: "bun" });
    assert.deepEqual(argv, [paths.bun, "build", "--compile", "--target=bun", "--compile-executable-path=" + paths.bun, "--no-compile-autoload-dotenv", "--no-compile-autoload-bunfig", "--no-compile-autoload-tsconfig", "--no-compile-autoload-package-json", "--no-env-file", "--no-install", "--config=" + paths.config, "--outfile=" + paths.output, paths.entry]);
    assert.throws(() => compilerArgv(paths, { os: "linux", arch: "arm64", bun_target: "bun" }), /build_target_rejected/);
    assert.throws(() => compilerArgv({ ...paths, entry: "../entry.ts" }, { os: "darwin", arch: "arm64", bun_target: "bun" }), /build_argv_path_rejected/);
    const before = await readdir(stage);
    await assert.rejects(planBuild({ staging: stage }), /build_closure_unavailable/);
    assert.deepEqual(await readdir(stage), before);
    assert.deepEqual(await readFile(source), content);
    const lock = JSON.parse(await readFile(new URL("./source-lock.json", import.meta.url)));
    assert.deepEqual(lock.entry, {
      path: "scripts/rcli/entry.ts",
      sha256: "93f81817b0eae962d767a1bc8a26958d7f12dc6b7660e41ac1d12b113e588156",
      bytes: 489,
      executable: true,
      contract: {
        path: "scripts/rcli/host.entry.test.mjs",
        sha256: "3b7ea0b7230dc5f1430ff35b2b70b92cfbe118e748192eb3d982051f7855b48d",
        bytes: 10195,
      },
    });
    assert.equal(lock.closure.complete, false);
    assert.equal(lock.compiler_arguments_qualified, false);
    const host = await readFile(new URL("./host.ts", import.meta.url));
    assert.deepEqual(lock.host_files.filter((row) => row.path.endsWith("/host.ts")), [{
      path: "scripts/rcli/host.ts",
      sha256: createHash("sha256").update(host).digest("hex"),
      bytes: host.length,
    }]);
  } finally {
    await rm(root, { recursive: true });
  }
});
