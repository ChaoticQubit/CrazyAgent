import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      for (const extension of [".ts", ".tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) {
      return nextLoad(url, context);
    }
    const result = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(result.errors, [], `production module did not transform: ${url}`);
    return { format: "module", source: result.code, shortCircuit: true };
  },
});

test("busy bridge calls are retried and other failures are not", async () => {
  const { retryBusy } = await import("../src/engine-client.ts");
  let calls = 0;
  const value = await retryBusy(async () => {
    calls += 1;
    if (calls < 3) throw { code: "busy" };
    return "read";
  }, 1);
  assert.equal(value, "read");
  assert.equal(calls, 3);
  calls = 0;
  await assert.rejects(retryBusy(async () => { calls += 1; throw { code: "stale_master" }; }, 1), { code: "stale_master" });
  assert.equal(calls, 1);
  calls = 0;
  await assert.rejects(retryBusy(async () => { calls += 1; throw { code: "busy" }; }, 1), { code: "busy" });
  assert.equal(calls, 8);
});

test("a burst of calls refused busy together does not retry in step until some give up", async () => {
  const { retryBusy } = await import("../src/engine-client.ts");
  let held = false;
  const hold = async () => {
    if (held) throw { code: "busy" };
    held = true;
    await new Promise((resolve) => setTimeout(resolve, 2));
    held = false;
    return "read";
  };
  const burst = Array.from({ length: 13 }, () => retryBusy(hold, 50));
  const settled = await Promise.allSettled(burst);
  assert.deepEqual(settled.filter((result) => result.status === "rejected").map((result) => result.reason), []);
});

test("a master opened before an engine or settings change explains how to recover", async () => {
  const { errorMessage } = await import("../src/engine-client.ts");
  assert.equal(errorMessage({ code: "master_source_conflict" }),
    "This master was opened before the engine or settings changed. Close it, start it again, then send.");
});
