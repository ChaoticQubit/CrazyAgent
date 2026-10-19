import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { lstatSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createNavigationReader } from "./refresh-barrier.mjs";
import { createRunDirectories } from "./run-layout.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const preload = new URL("./private-log-preload.mjs", import.meta.url);
const writerSource = `
import logger from "@wdio/logger";
import { LogWriter } from "@wdio/native-core";
import { enableFileLogging } from "@wdio/utils";

await enableFileLogging(process.argv[2]);
logger.setLogLevelsConfig({}, "info");
logger("private-log-test").info("ordinary private log diagnostic");
const writer = new LogWriter("private-log-test");
writer.initialize(process.argv[2]);
writer.write("ordinary private log diagnostic");
await writer.close();
await logger.waitForBuffer();
logger.clearLogger();
console.log(JSON.stringify({ logFile: writer.getLogFile() }));
`;

test("installed WDIO writers create private logs without changing the parent mask", () => {
  const mask = process.umask(0o022);
  let evidence;
  try {
    evidence = mkdtempSync(join(tmpdir(), "ca-log-producers-"));
    createRunDirectories(evidence);
    const directory = join(evidence, "webdriver");
    const result = spawnSync(process.execPath,
      ["--import", preload.href, "--input-type=module", "-", directory], {
        cwd: appRoot,
        env: { HOME: join(evidence, "home"), TMPDIR: join(evidence, "tmp"),
          LANG: "en_US.UTF-8", NO_COLOR: "1" },
        input: writerSource, encoding: "utf8", timeout: 10000,
        maxBuffer: 64 * 1024, killSignal: "SIGKILL",
      });
    assert.ifError(result.error);
    assert.equal(result.signal, null);
    assert.equal(result.status, 0, result.stderr);
    const { logFile } = JSON.parse(result.stdout);
    assert.equal(dirname(logFile), directory);
    assert.match(basename(logFile), /^wdio-[A-Za-z0-9-]+\.log$/);
    const names = readdirSync(directory).sort();
    assert.deepEqual(names, ["wdio.log", basename(logFile)].sort());
    const modes = [];
    const files = names.map((name) => {
      const path = join(directory, name);
      const info = lstatSync(path);
      assert(info.isFile());
      assert.equal(info.isSymbolicLink(), false);
      assert.equal(info.uid, process.getuid());
      assert(info.size > 0, "installed writer must produce a nonempty log");
      assert(readFileSync(path, "utf8").includes("ordinary private log diagnostic"));
      modes.push(info.mode & 0o777);
      return { name, bytes: info.size, mode: (info.mode & 0o777).toString(8), uid: info.uid };
    });
    for (const path of [evidence, ...["home", "tmp", "webdriver"].map((name) => join(evidence, name))]) {
      const info = lstatSync(path);
      assert(info.isDirectory());
      assert.equal(info.isSymbolicLink(), false);
      assert.equal(info.uid, process.getuid());
      assert.equal(info.mode & 0o777, 0o700);
    }
    assert.equal(process.umask(0o022), 0o022, "child must preserve the parent mask");
    console.log(JSON.stringify({ files, directory_mode: "700", parent_mask: "22" }));
    assert.deepEqual(modes, [0o600, 0o600], "both installed writers must create private logs");
    assert.equal(createNavigationReader(evidence, "log-policy-338")().sequence, 0);
  } finally {
    assert.equal(process.umask(mask), 0o022, "test must restore its original parent mask");
    if (evidence) rmSync(evidence, { recursive: true });
  }
});
