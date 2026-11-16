import assert from "node:assert/strict";
import { basename, dirname, isAbsolute, normalize } from "node:path";
import type { EventBus } from "../../.local-state/tools/rcli-source/src/core/bus/EventBus.ts";
import type { McpConfig } from "../../.local-state/tools/rcli-source/src/core/mcp/config.ts";
import type { McpClientManager } from "../../.local-state/tools/rcli-source/src/core/mcp/MCPClientManager.ts";
import type { PermissionRule } from "../../.local-state/tools/rcli-source/src/core/permissions/PermissionRules.ts";
import type { PermissionContext } from "../../.local-state/tools/rcli-source/src/core/permissions/types.ts";
import type { ToolRegistry } from "../../.local-state/tools/rcli-source/src/core/tools/ToolRegistry.ts";

/** The one connector server a granted turn names: the engine executable and the turn's launch folder. */
export type Gateway = { engine: string; folder: string };

export const GATEWAY_SERVER = "ca-connectors";
export const GATEWAY_COMMAND = "connector-mcp";
export const GATEWAY_LAUNCHES = "connector-launches";
export const GATEWAY_TIMEOUT_MS = 2_400_000;
export const GATEWAY_CONNECT_MS = 10_000;
export const GATEWAY_TOOLS_MAX = 512;

/**
 * Checks the two gateway paths a launch names: absolute, normal and plain, the folder directly
 * in a `connector-launches` folder, and neither inside the home or the worktree. Inputs: the
 * engine, the folder, the home and the worktree. Output: the gateway, or a throw.
 */
export function checkGateway(engine: string, folder: string, homeDir: string, cwd: string): Gateway {
  for (const path of [engine, folder]) {
    if (!path.isWellFormed() || path.includes("\0") || Buffer.byteLength(path) > 4096 || !isAbsolute(path) || normalize(path) !== path || path === "/") throw new Error("host_gateway_rejected");
    if ([homeDir, cwd].some((inside) => path === inside || path.startsWith(`${inside}/`))) throw new Error("host_gateway_rejected");
  }
  if (engine === folder || basename(dirname(folder)) !== GATEWAY_LAUNCHES) throw new Error("host_gateway_rejected");
  assert(isAbsolute(engine) && isAbsolute(folder), "host_gateway_invariant");
  return { engine, folder };
}

/**
 * Builds the rcli MCP config of the one gateway server, reading no MCP file and passing no
 * environment; the gateway's read-only annotations are trusted so manual mode runs its read
 * tools with no prompt. Input: a checked gateway. Output: the config.
 */
export function gatewayConfig(gateway: Gateway): McpConfig {
  assert(isAbsolute(gateway.engine) && isAbsolute(gateway.folder), "host_gateway_invariant");
  const config: McpConfig = {
    timeoutMs: GATEWAY_TIMEOUT_MS,
    warnings: [],
    servers: [{
      name: GATEWAY_SERVER, type: "stdio", disabled: false, configSources: [],
      command: gateway.engine, args: [GATEWAY_COMMAND, gateway.folder], env: {}, headers: {},
      disabledTools: [], disabledPrompts: [], disabledResources: [],
      timeoutMs: GATEWAY_TIMEOUT_MS, trustToolAnnotations: true,
    }],
  };
  assert(config.servers.length === 1, "host_gateway_invariant");
  return config;
}

/**
 * The gateway's tools in one host's registry: connected once, at the first turn start, and
 * closed with the host. Each listed tool gets the host's allow rule, so a write reaches the
 * gateway, which holds it for the owner's answer. A gateway that does not answer within
 * `GATEWAY_CONNECT_MS`, or lists more than `GATEWAY_TOOLS_MAX` tools, gives no tool or rule.
 */
export class GatewayTools {
  names: readonly string[] = [];
  private readonly gateway: Gateway;
  private readonly cwd: string;
  private readonly registry: ToolRegistry;
  private readonly bus: EventBus;
  private readonly permissions: PermissionContext;
  private manager: McpClientManager | null = null;
  private started = false;

  constructor(gateway: Gateway, cwd: string, registry: ToolRegistry, bus: EventBus, permissions: PermissionContext) {
    assert(isAbsolute(cwd), "host_gateway_invariant");
    assert(isAbsolute(gateway.engine) && isAbsolute(gateway.folder), "host_gateway_invariant");
    this.gateway = gateway;
    this.cwd = cwd;
    this.registry = registry;
    this.bus = bus;
    this.permissions = permissions;
  }

  async sync(signal: AbortSignal): Promise<void> {
    if (this.started) return;
    this.started = true;
    assert(this.names.length === 0 && this.manager === null, "host_gateway_invariant");
    try {
      const [{ McpClientManager }, { McpToolRegistrar }] = await Promise.all([
        import("../../.local-state/tools/rcli-source/src/core/mcp/MCPClientManager.ts"),
        import("../../.local-state/tools/rcli-source/src/core/mcp/MCPToolRegistrar.ts"),
      ]);
      this.manager = new McpClientManager(gatewayConfig(this.gateway), this.cwd);
      const result = await this.manager.initialize(AbortSignal.any([signal, AbortSignal.timeout(GATEWAY_CONNECT_MS)]));
      const tools = result.tools.filter((tool) => tool.serverName === GATEWAY_SERVER);
      if (tools.length !== result.tools.length || tools.length > GATEWAY_TOOLS_MAX) throw new Error("host_gateway_rejected");
      const names = new McpToolRegistrar(this.registry, this.bus).register({ tools, warnings: [] }).toolNames;
      const rules = names.map((name): PermissionRule => {
        const tool = this.registry.get(name);
        assert(tool !== undefined && name.startsWith(`mcp__${GATEWAY_SERVER}__`), "host_gateway_invariant");
        return { behavior: "allow", toolName: tool.agentName.toLowerCase(), raw: tool.agentName };
      });
      this.names = names;
      this.permissions.rules.allow.push(...rules);
    } catch {
      this.names = [];
    }
    assert(this.names.length <= GATEWAY_TOOLS_MAX, "host_gateway_invariant");
    assert(this.names.every((name) => this.permissions.rules.allow.some((rule) => rule.toolName === name.toLowerCase())), "host_gateway_invariant");
  }

  async close(): Promise<void> {
    const manager = this.manager;
    this.manager = null;
    this.started = true;
    await manager?.close();
    assert(this.manager === null, "host_gateway_invariant");
  }
}
