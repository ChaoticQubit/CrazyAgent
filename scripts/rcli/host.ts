export * from "./host.codec.ts";
export { type PrivateKeyWriteBinding, runPrivateProviderKeyAction, writePrivateProviderKey } from "./host.keys.ts";
import { identifier } from "./host.codec.ts";
import {
  AgentController,
  type AgentControllerDeps,
} from "../../.local-state/tools/rcli-source/src/core/agent/AgentController.ts";
import { EventBus } from "../../.local-state/tools/rcli-source/src/core/bus/EventBus.ts";
import { Session } from "../../.local-state/tools/rcli-source/src/core/session/Session.ts";
import { SkillController } from "../../.local-state/tools/rcli-source/src/core/skills/SkillController.ts";
import { ToolRegistry } from "../../.local-state/tools/rcli-source/src/core/tools/ToolRegistry.ts";

export function assembleClosedController(input: Pick<AgentControllerDeps, "config" | "client" | "permissions"> & { sessionId: string; cwd: string; homeDir: string }): { controller: AgentController; bus: EventBus; session: Session } {
  const sessionId = identifier(input.sessionId);
  const bus = new EventBus();
  const session = new Session(sessionId);
  const registry = new ToolRegistry();
  const skillController = new SkillController({ cwd: input.cwd, homeDir: input.homeDir, bus });
  const controller = new AgentController({ config: input.config, client: input.client, permissions: input.permissions, bus, session, registry, skillController });
  return { controller, bus, session };
}
