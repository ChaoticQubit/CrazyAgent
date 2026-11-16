import { expect, test } from "bun:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { Config } from "../../.local-state/tools/rcli-source/src/config/Config.ts";
import type { AgentControllerDeps } from "../../.local-state/tools/rcli-source/src/core/agent/AgentController.ts";
import { EventBus } from "../../.local-state/tools/rcli-source/src/core/bus/EventBus.ts";
import type { AgentEvent } from "../../.local-state/tools/rcli-source/src/core/bus/events.ts";
import { buildPermissionContext } from "../../.local-state/tools/rcli-source/src/core/permissions/index.ts";
import { decidePermission } from "../../.local-state/tools/rcli-source/src/core/permissions/PermissionEngine.ts";
import { ToolRegistry } from "../../.local-state/tools/rcli-source/src/core/tools/ToolRegistry.ts";
import type { ProviderEvent, SendMessageRequest, SubmitToolOutputsRequest } from "../../.local-state/tools/rcli-source/src/providers/backboard/types.ts";
import { assembleClosedController } from "./host.ts";
import {
  GATEWAY_COMMAND, GATEWAY_CONNECT_MS, GATEWAY_LAUNCHES, GATEWAY_SERVER, GATEWAY_TIMEOUT_MS,
  GATEWAY_TOOLS_MAX, GatewayTools, gatewayConfig, type Gateway,
} from "./host.mcp.ts";
import { normalizeSdkObservation } from "./host.observations.ts";
import { admitLaunch, parseLaunch } from "./host.runtime.ts";

const LIST = "mcp__ca-connectors__linear_list_issues";
const CREATE = "mcp__ca-connectors__linear_create_issue";

type Fixture = { cwd: string; home: string; gateway: Gateway; calls: string };

/**
 * Writes a stand-in for the engine's `connector-mcp` server: a stdio MCP server that answers
 * only when started as `connector-mcp <folder>`, lists one read tool and one write tool and
 * logs each call it gets. Inputs: the folder and the call log. Output: the script text.
 */
function stubServer(folder: string, calls: string): string {
  return `
import { appendFileSync } from "node:fs";
import { createInterface } from "node:readline";
if (process.argv.length !== 4 || process.argv[2] !== ${JSON.stringify(GATEWAY_COMMAND)} || process.argv[3] !== ${JSON.stringify(folder)}) process.exit(3);
const tools = [
  { name: "linear_list_issues", description: "List Linear issues", inputSchema: { type: "object", properties: {} }, annotations: { readOnlyHint: true } },
  { name: "linear_create_issue", description: "Create a Linear issue", inputSchema: { type: "object", properties: {} } },
];
const send = (message) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...message }) + "\\n");
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  if (request.method === "initialize") send({ id: request.id, result: { protocolVersion: request.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "stub", version: "1.0.0" } } });
  else if (request.method === "tools/list") send({ id: request.id, result: { tools } });
  else if (request.method === "tools/call") {
    appendFileSync(${JSON.stringify(calls)}, request.params.name + "\\n");
    send({ id: request.id, result: { content: [{ type: "text", text: "ISSUE-77 open" }] } });
  } else send({ id: request.id, error: { code: -32601, message: "unknown method" } });
}
`;
}

/**
 * Runs one row in a private temp root holding a worktree, a home, an engine root with the
 * turn's launch folder and an engine executable. Inputs: the row and the engine script.
 * Output: the row's result; the root is removed after it.
 */
async function withFixture(run: (fixture: Fixture) => Promise<void>, server = stubServer): Promise<void> {
  const root = await realpath(await mkdtemp(join(tmpdir(), "rcli-gateway-")));
  try {
    await chmod(root, 0o700);
    const cwd = join(root, "project");
    const home = join(root, "home");
    const folder = join(root, "engine-root", GATEWAY_LAUNCHES, "launch-a");
    for (const path of [join(cwd, ".git"), home, folder]) await mkdir(path, { recursive: true, mode: 0o700 });
    const engine = join(root, "engine.mjs");
    const calls = join(root, "calls.log");
    await writeFile(engine, `#!${process.execPath}\n${server(folder, calls)}`, { mode: 0o700 });
    assert(engine.startsWith(root) && !engine.startsWith(cwd));
    await run({ cwd, home, gateway: { engine, folder }, calls });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

/** A model stand-in that plays two scripted streams: the first turn, then the continuation after tool results. */
class ScriptedClient {
  readonly capabilities = { assistants: false, threads: true, memory: false };
  readonly messages: SendMessageRequest[] = [];
  readonly outputs: SubmitToolOutputsRequest[] = [];

  constructor(private readonly first: ProviderEvent[], private readonly second: ProviderEvent[]) {}

  async listAssistants(): Promise<never[]> { return []; }
  async createAssistant(): Promise<never> { throw new Error("no assistant records"); }
  async getModelThinkingMetadata(provider: string, model: string) {
    return { provider, model, supports_thinking: false, thinking_controls: { supported: false, allowed_fields: [], defaults_only: true } };
  }
  async listModels() { return { models: [], total: 0 }; }
  sourceForThread(): "byok" { return "byok"; }
  async *runMessage(request: SendMessageRequest): AsyncIterable<ProviderEvent> {
    this.messages.push(request);
    yield* this.first;
  }
  async *runToolOutputs(request: SubmitToolOutputsRequest): AsyncIterable<ProviderEvent> {
    this.outputs.push(request);
    yield* this.second;
  }
  async preserveFailedToolOutputs(): Promise<null> { return null; }
}

test("the host builds the one gateway server itself, trusting its annotations, with no environment and no MCP file", () => {
  const gateway = { engine: "/engine/crazyagent", folder: "/root/connector-launches/launch-a" };
  expect(gatewayConfig(gateway)).toEqual({
    timeoutMs: GATEWAY_TIMEOUT_MS,
    warnings: [],
    servers: [{
      name: GATEWAY_SERVER, type: "stdio", disabled: false, configSources: [],
      command: gateway.engine, args: [GATEWAY_COMMAND, gateway.folder], env: {}, headers: {},
      disabledTools: [], disabledPrompts: [], disabledResources: [],
      timeoutMs: GATEWAY_TIMEOUT_MS, trustToolAnnotations: true,
    }],
  });
});

test("the host's gateway words and waits match the engine's shim, gateway and launch entry", async () => {
  const source = (path: string) => readFile(new URL(`../../crates/crazyagent/src/connectors/${path}`, import.meta.url), "utf8");
  const shim = await source("shim.rs");
  expect(shim).toContain(`pub const SERVER_NAME: &str = "${GATEWAY_SERVER}";`);
  expect(shim).toContain(`pub const SERVER_COMMAND: &str = "${GATEWAY_COMMAND}";`);
  expect(await source("launch.rs")).toContain(`pub const SERVER_NAME: &str = "${GATEWAY_SERVER}";`);
  expect(await source("gateway.rs")).toContain(`pub const LAUNCHES_FOLDER: &str = "${GATEWAY_LAUNCHES}";`);
  const wait = /pub\(crate\) const REPLY_WAIT: Duration = Duration::from_secs\((\d+)\);/.exec(shim);
  assert(wait !== null, "the shim names its reply wait");
  expect(GATEWAY_TIMEOUT_MS).toBeGreaterThan(Number(wait[1]) * 1000);
  expect(GATEWAY_CONNECT_MS).toBeLessThanOrEqual(10_000);
  expect(GATEWAY_TOOLS_MAX).toBeGreaterThan(0);
});

test("a launch names the gateway with both flags or neither and refuses half, a repeat, a bad path or any other MCP source", () => {
  const base = ["--session-id", "session-a", "--instance-id", "instance-a", "--home-dir", "/r/home", "--cwd", "/r/project"];
  const engine = "/r/engine/crazyagent";
  const folder = "/r/engine-root/connector-launches/launch-a";
  const flags = ["--gateway-engine", engine, "--gateway-folder", folder];
  expect(() => parseLaunch([...base, ...flags])).not.toThrow();
  expect(parseLaunch([...base, ...flags]).gateway).toEqual({ engine, folder });
  const whole = [...base, ...flags, "--instructions-file", "/r/run/instructions.md", "--model", "p/m"];
  expect(parseLaunch(whole).gateway).toEqual({ engine, folder });
  expect(parseLaunch(whole).instructionsFile).toBe("/r/run/instructions.md");
  expect(parseLaunch(base).gateway).toBeUndefined();
  const named = (engineAt: string, folderAt: string) => [...base, "--gateway-engine", engineAt, "--gateway-folder", folderAt];
  for (const argv of [
    [...base, "--gateway-engine", engine],
    [...base, "--gateway-folder", folder],
    [...base, ...flags, "--gateway-folder", folder],
    [...base, ...flags, "--gateway-engine", engine, "--gateway-folder", folder],
    named("crazyagent", folder),
    named(engine, "launch-a"),
    named(engine, "/r/engine-root/other/launch-a"),
    named(engine, "/r/engine-root/connector-launches"),
    named(engine, "/r/engine-root/connector-launches/../launch-a"),
    named(engine, "/r/project/connector-launches/launch-a"),
    named(engine, "/r/home/connector-launches/launch-a"),
    named("/r/project/crazyagent", folder),
    named("/r/home/crazyagent", folder),
    named(folder, folder),
    [...base, ...flags, "--mcp-config", "/r/mcp.json"],
  ]) expect(() => parseLaunch(argv)).toThrow("host_launch_rejected");
});

test("the host admits only a private gateway folder reached without a link and an engine file that exists", async () => {
  await withFixture(async ({ cwd, home, gateway }) => {
    const launch = { sessionId: "session-a", instanceId: "instance-a", homeDir: home, cwd, gateway };
    await admitLaunch(launch, home);
    const open = join(dirname(gateway.folder), "launch-open");
    await mkdir(open, { mode: 0o700 });
    await chmod(open, 0o755);
    const linked = join(dirname(gateway.folder), "launch-linked");
    await symlink(gateway.folder, linked);
    for (const changed of [
      { ...gateway, folder: open },
      { ...gateway, folder: linked },
      { ...gateway, folder: join(dirname(gateway.folder), "launch-missing") },
      { ...gateway, engine: join(dirname(gateway.engine), "missing-engine") },
    ]) await expect(admitLaunch({ ...launch, gateway: changed }, home)).rejects.toThrow("host_launch_rejected");
  });
}, 20_000);

test("a granted host lists the gateway's tools once, a read tool runs in manual mode with no prompt and a write tool asks", async () => {
  await withFixture(async ({ cwd, gateway }) => {
    const registry = new ToolRegistry();
    const tools = new GatewayTools(gateway, cwd, registry, new EventBus());
    try {
      await tools.sync(AbortSignal.timeout(GATEWAY_CONNECT_MS));
      expect(registry.list().map((tool) => tool.name).sort()).toEqual([CREATE, LIST]);
      expect([...tools.names].sort()).toEqual([CREATE, LIST]);
      const permissions = buildPermissionContext(cwd, "manual", false);
      expect(decidePermission(registry.get(LIST)!, {}, permissions, cwd).behavior).toBe("allow");
      expect(decidePermission(registry.get(CREATE)!, {}, permissions, cwd).behavior).toBe("ask");
      await tools.sync(AbortSignal.timeout(GATEWAY_CONNECT_MS));
      expect(registry.list()).toHaveLength(2);
    } finally {
      await tools.close();
    }
  });
}, 20_000);

test("a gateway that does not start leaves the turn without connector tools and fails nothing", async () => {
  await withFixture(async ({ cwd, gateway }) => {
    const registry = new ToolRegistry();
    const tools = new GatewayTools(gateway, cwd, registry, new EventBus());
    try {
      await tools.sync(AbortSignal.timeout(GATEWAY_CONNECT_MS));
      expect(registry.list()).toEqual([]);
      expect(tools.names).toEqual([]);
    } finally {
      await tools.close();
    }
  }, () => "process.exit(1);\n");
}, 20_000);

test("a granted master turn calls a gateway read tool, the write tool is refused before the server, and every event is accepted", async () => {
  await withFixture(async ({ cwd, home, gateway, calls }) => {
    const client = new ScriptedClient(
      [{ kind: "thread", threadId: "thr_gateway" }, { kind: "requires_action", runId: "run_1", calls: [
        { id: "c1", name: LIST, input: {} },
        { id: "c2", name: CREATE, input: {} },
      ] }],
      [{ kind: "assistant_delta", text: "ISSUE-77 is open" }, { kind: "completed" }],
    );
    const config = new Config({ env: { apiKey: "k", apiUrl: "https://example.test/api" }, argv: ["--cwd", cwd], homeDir: home });
    const permissions = buildPermissionContext(config.cwd, "manual", false);
    const assembled = assembleClosedController({
      config, permissions, sessionId: "gateway-turn", cwd: config.cwd, homeDir: home, gateway,
      client: client as unknown as AgentControllerDeps["client"],
    });
    const events: AgentEvent[] = [];
    const detach = assembled.bus.onAny((event) => events.push(event));
    try {
      expect(await assembled.controller.submit("list my Linear issues")).toBe("completed");
      const offered = (client.messages[0]?.tools ?? []).map((tool) => tool.function.name);
      expect(offered).toContain(LIST);
      expect(offered).toContain(CREATE);
      const outputs = new Map(client.outputs[0]?.tool_outputs.map((output) => [output.tool_call_id, output.output]));
      expect(outputs.get("c1")).toContain("ISSUE-77 open");
      expect(outputs.get("c2") ?? "").not.toContain("ISSUE-77");
      expect(await readFile(calls, "utf8")).toBe("linear_list_issues\n");
      expect(events.some((event) => event.type === "tool:result" && event.name === LIST)).toBe(true);
      for (const event of events) expect(() => normalizeSdkObservation(event)).not.toThrow();
    } finally {
      detach();
      await assembled.gateway?.close();
      await assembled.controller.dispose();
    }
  });
}, 30_000);

test("a host without a gateway has no tools and no gateway to close", async () => {
  await withFixture(async ({ cwd, home }) => {
    const config = new Config({ env: { apiKey: "k", apiUrl: "https://example.test/api" }, argv: ["--cwd", cwd], homeDir: home });
    const assembled = assembleClosedController({
      config, permissions: buildPermissionContext(config.cwd, "manual", false), sessionId: "no-gateway", cwd: config.cwd, homeDir: home,
      client: new ScriptedClient([], []) as unknown as AgentControllerDeps["client"],
    });
    try {
      expect(assembled.controller.listToolNames()).toEqual([]);
      expect(assembled.gateway).toBeNull();
    } finally {
      await assembled.controller.dispose();
    }
  });
});
