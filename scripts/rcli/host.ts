export * from "./host.codec.ts";
export { type PrivateKeyWriteBinding, runPrivateProviderKeyAction, writePrivateProviderKey } from "./host.keys.ts";
import assert from "node:assert/strict";
import { identifier } from "./host.codec.ts";
import {
  AgentController,
  type AgentControllerDeps,
} from "../../.local-state/tools/rcli-source/src/core/agent/AgentController.ts";
import { EventBus } from "../../.local-state/tools/rcli-source/src/core/bus/EventBus.ts";
import { Session } from "../../.local-state/tools/rcli-source/src/core/session/Session.ts";
import { SkillController } from "../../.local-state/tools/rcli-source/src/core/skills/SkillController.ts";
import { ToolRegistry } from "../../.local-state/tools/rcli-source/src/core/tools/ToolRegistry.ts";
import { GatewayTools, type Gateway } from "./host.mcp.ts";

export function assembleClosedController(input: Pick<AgentControllerDeps, "config" | "client" | "permissions" | "startupEnvironmentPrompt"> & { sessionId: string; cwd: string; homeDir: string; gateway?: Gateway; notice?: (text: string) => void }): { controller: AgentController; bus: EventBus; session: Session; gateway: GatewayTools | null } {
  const sessionId = identifier(input.sessionId);
  const bus = new EventBus();
  const session = new Session(sessionId);
  const registry = new ToolRegistry();
  const skillController = new SkillController({ cwd: input.cwd, homeDir: input.homeDir, bus });
  assert((input.gateway === undefined) === (input.notice === undefined), "host_gateway_invariant");
  const notice = input.notice;
  const gateway = input.gateway === undefined ? null : new GatewayTools(input.gateway, input.cwd, registry, bus, input.permissions);
  const syncDynamicTools = gateway === null || notice === undefined ? {} : { syncDynamicTools: async (signal: AbortSignal) => {
    await gateway.sync(signal);
    const line = gateway.takeNotice();
    if (line !== null) notice(line);
  } };
  assert((gateway === null) === (input.gateway === undefined), "host_gateway_invariant");
  const controller = new AgentController({ config: input.config, client: input.client, permissions: input.permissions, bus, session, registry, skillController, startupEnvironmentPrompt: input.startupEnvironmentPrompt, ...syncDynamicTools });
  return { controller, bus, session, gateway };
}
