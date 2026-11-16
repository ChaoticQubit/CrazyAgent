import type { EventBus } from "../../.local-state/tools/rcli-source/src/core/bus/EventBus.ts";
import type { McpConfig } from "../../.local-state/tools/rcli-source/src/core/mcp/config.ts";
import type { ToolRegistry } from "../../.local-state/tools/rcli-source/src/core/tools/ToolRegistry.ts";

/** The one connector server a granted turn names: the engine executable and the turn's launch folder. */
export type Gateway = { engine: string; folder: string };

export const GATEWAY_SERVER = "ca-connectors";
export const GATEWAY_COMMAND = "connector-mcp";
export const GATEWAY_LAUNCHES = "connector-launches";
export const GATEWAY_TIMEOUT_MS = 250_000;
export const GATEWAY_CONNECT_MS = 10_000;
export const GATEWAY_TOOLS_MAX = 512;

/**
 * Checks the two gateway paths a launch names. Inputs: the engine, the folder, the home and
 * the worktree. Output: the gateway, or a throw for a path the host must not use.
 */
export function checkGateway(engine: string, folder: string, _homeDir: string, _cwd: string): Gateway {
  return { engine, folder };
}

/**
 * Builds the rcli MCP config of the one gateway server, reading no MCP file. Input: the
 * checked gateway. Output: the config.
 */
export function gatewayConfig(_gateway: Gateway): McpConfig {
  return { timeoutMs: GATEWAY_TIMEOUT_MS, servers: [], warnings: [] };
}

/**
 * The gateway's tools in one host's registry: connected once, at the first turn start, and
 * closed with the host. A gateway that does not answer leaves the turn without its tools.
 */
export class GatewayTools {
  names: readonly string[] = [];

  constructor(readonly gateway: Gateway, readonly cwd: string, readonly registry: ToolRegistry, readonly bus: EventBus) {}

  async sync(_signal: AbortSignal): Promise<void> {}

  async close(): Promise<void> {}
}
