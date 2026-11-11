import { readFileSync, statSync } from "node:fs";
import * as vscode from "vscode";
import { LOCKED_BYTES_MAX, parseLocked, restores, type Seen } from "./lock-model.js";

/** The variable naming the engine's locked-keys file to the extension host. */
export const LOCKED_ENV = "CRAZYAGENT_IDE_LOCKED_FILE";

/**
 * Keeps the owner's User settings from contradicting the engine's lock. Remote settings and
 * the workspace file hold the lock; this writes back the application keys only User
 * settings apply, at start and whenever a locked key changes. Inputs: the extension context
 * and the environment. Output: none; nothing happens without a readable locked-keys file.
 */
export function startLock(context: vscode.ExtensionContext, env: Record<string, string | undefined>): void {
  const path = env[LOCKED_ENV];
  if (path === undefined || path === "") return;
  let read: Record<string, unknown> | undefined;
  try {
    read = statSync(path).size > LOCKED_BYTES_MAX ? undefined : parseLocked(readFileSync(path, "utf8"));
  } catch {
    return;
  }
  if (read === undefined) return;
  const locked = read;
  const keys = Object.keys(locked);
  const reassert = (): void => {
    const config = vscode.workspace.getConfiguration();
    const seen: Record<string, Seen> = {};
    for (const key of keys) seen[key] = { value: config.get(key), user: config.inspect(key)?.globalValue };
    for (const { key, value } of restores(locked, seen)) {
      Promise.resolve(config.update(key, value, vscode.ConfigurationTarget.Global)).catch(() => undefined);
    }
  };
  reassert();
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (keys.some((key) => event.affectsConfiguration(key))) reassert();
    }),
  );
}
