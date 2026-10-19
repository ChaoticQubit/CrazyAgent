import { expect, test } from "bun:test";
import assert from "node:assert/strict";
import { chmod, lstat, mkdir, mkdtemp, readdir, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { Config } from "../../.local-state/tools/rcli-source/src/config/Config.ts";
import { NO_CREDENTIALS_MESSAGE } from "../../.local-state/tools/rcli-source/src/config/auth.ts";
import { buildPermissionContext } from "../../.local-state/tools/rcli-source/src/core/permissions/index.ts";
import type { TodoItem } from "../../.local-state/tools/rcli-source/src/core/bus/events.ts";
import { createAgentClient } from "../../.local-state/tools/rcli-source/src/providers/createAgentClient.ts";
import { assembleClosedController } from "./host.ts";

function requireCleanAuthEnvironment(): void {
  if (
    process.env.BACKBOARD_API_KEY !== undefined ||
    process.env.BACKBOARD_API_URL !== undefined ||
    process.env.BACKBOARD_ALLOW_INSECURE_API_URL !== undefined
  ) {
    throw new Error("construction_clean_auth_environment_required");
  }
}

async function admittedHome(): Promise<string> {
  const home = process.env.CRAZYAGENT_RCLI_CONSTRUCTION_HOME;
  if (!home || !isAbsolute(home)) {
    throw new Error("construction_explicit_admitted_home_required");
  }
  const metadata = await lstat(home);
  if (
    !metadata.isDirectory() ||
    metadata.isSymbolicLink() ||
    metadata.uid !== process.getuid?.() ||
    (metadata.mode & 0o077) !== 0 ||
    await realpath(home) !== home
  ) {
    throw new Error("construction_private_canonical_home_required");
  }
  return home;
}

async function ownedFixture(run: (cwd: string, emptyHome: string) => Promise<void>): Promise<void> {
  const directory = await mkdtemp(join(tmpdir(), "rcli-construction-"));
  try {
    await chmod(directory, 0o700);
    const cwd = join(directory, "project");
    const emptyHome = join(directory, "empty-home");
    await mkdir(cwd, { mode: 0o700 });
    await mkdir(join(cwd, ".git"), { mode: 0o700 });
    await mkdir(emptyHome, { mode: 0o700 });
    assert(cwd !== emptyHome);
    await run(cwd, emptyHome);
  } finally {
    await rm(directory, { recursive: true });
  }
}

test("actual Config rejects an empty authentication home precisely", async () => {
  requireCleanAuthEnvironment();
  await ownedFixture(async (cwd, emptyHome) => {
    expect(await readdir(emptyHome)).toEqual([]);
    expect(() => new Config({ argv: ["--cwd", cwd], homeDir: emptyHome })).toThrow(
      new Error(NO_CREDENTIALS_MESSAGE),
    );
    expect(await readdir(emptyHome)).toEqual([]);
    expect(await readdir(cwd)).toEqual([".git"]);
  });
}, 10_000);

test("actual admitted Config and client construct a closed controller whose disposal detaches session listeners", async () => {
  requireCleanAuthEnvironment();
  const homeDir = await admittedHome();
  await ownedFixture(async (cwd) => {
    const config = new Config({ argv: ["--cwd", cwd], homeDir });
    expect(config.hasBackendForCurrentModel).toBe(true);
    expect(config.cwd).toBe(cwd);
    const client = createAgentClient(config);
    const permissions = buildPermissionContext(config.cwd, "manual", false);
    expect(permissions).toEqual({
      mode: "manual",
      interactive: false,
      rules: { allow: [], deny: [], ask: [] },
    });
    const { controller, bus, session } = assembleClosedController({
      config,
      client,
      permissions,
      sessionId: "construction-checkpoint",
      cwd: config.cwd,
      homeDir,
    });
    const before: TodoItem[] = [{ id: "before-dispose", content: "local projection fixture", status: "completed" }];
    const after: TodoItem[] = [{ id: "after-dispose", content: "detached projection fixture", status: "pending" }];
    try {
      expect(controller.listToolNames()).toEqual([]);
      expect(controller.permissionMode).toBe("manual");
      expect(controller.isRunning).toBe(false);
      expect(controller.hasActiveWork).toBe(false);
      expect(controller.threadId).toBeNull();
      expect(session.sessionId).toBe("construction-checkpoint");
      expect(session.getMessages()).toEqual([]);
      expect(config.isSkillDiscoveryEnabled).toBe(false);
      expect(config.isBrowserUseEnabled).toBe(false);
      expect(config.isComputerUseEnabled).toBe(false);
      bus.emit({ type: "todos:updated", todos: before });
      expect(session.todos).toEqual(before);
      bus.emit({ type: "turn:start", turnId: "local-before-dispose" });
      expect(session.todos).toEqual([]);
      bus.emit({ type: "todos:updated", todos: before });
      expect(session.todos).toEqual(before);
    } finally {
      await controller.dispose();
    }
    expect(controller.isRunning).toBe(false);
    expect(controller.hasActiveWork).toBe(false);
    bus.emit({ type: "todos:updated", todos: after });
    expect(session.todos).toEqual(before);
    bus.emit({ type: "turn:start", turnId: "local-after-dispose" });
    expect(session.todos).toEqual(before);
    expect(await readdir(cwd)).toEqual([".git"]);
  });
}, 10_000);
