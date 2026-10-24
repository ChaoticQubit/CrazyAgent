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

const client = await import("../src/engine-client.ts");

test("setup and default-pack refusals read as plain sentences", () => {
  const fallback = client.errorMessage({ code: "unknown" });
  assert.equal(client.errorMessage({ code: "setup_busy" }), "Setup is already running.");
  assert.equal(client.errorMessage({ code: "pack_required" }), "This entry is always on and can't be removed.");
  for (const code of ["setup_store_unavailable", "setup_outcome_unknown", "pack_unknown", "pack_not_installed", "pack_name_taken"]) {
    assert.notEqual(client.errorMessage({ code }), fallback, code);
  }
});

test("every setup command the client calls is a desktop command the bridge maps plainly", () => {
  const source = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  const bridge = readFileSync(new URL("../src-tauri/src/bridge.rs", import.meta.url), "utf8");
  const setup = readFileSync(new URL("../src-tauri/src/bridge/setup.rs", import.meta.url), "utf8");
  const commands = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const expected = {
    setupStatus: "desktop_setup_status",
    runSetup: "desktop_run_setup",
    setPackEntry: "desktop_set_pack_entry",
    removePackEntry: "desktop_remove_pack_entry",
  };
  for (const [name, command] of Object.entries(expected)) {
    assert.equal(typeof client[name], "function", name);
    const match = source.match(new RegExp(`export function ${name}\\([^)]*\\)[^{]*\\{\\s*return invoke<[^>]+>\\("(desktop_[a-z_]+)"`));
    assert.ok(match, `${name} must invoke a desktop command`);
    assert.equal(match[1], command);
    assert.ok(commands.includes(`async fn ${command}(`), command);
  }
  for (const name of ["run_setup", "set_pack_entry", "remove_pack_entry"]) {
    assert.match(setup, new RegExp(`pub async fn ${name}\\([^)]*generation: u64,?\\s*\\) -> Result<IntentReceipt, BridgeError>`), name);
  }
  for (const kind of ["run_setup", "pack_set", "pack_remove"]) {
    assert.ok(bridge.includes(`=> ("${kind}", None, None)`), kind);
    assert.ok(source.includes(`"${kind}"`), kind);
  }
  for (const code of ["setup_busy", "pack_unknown", "pack_required", "pack_not_installed", "pack_name_taken"]) {
    assert.ok(bridge.includes(`"${code}"`), code);
    assert.notEqual(client.errorMessage({ code }), client.errorMessage({ code: "unknown" }), code);
  }
});
