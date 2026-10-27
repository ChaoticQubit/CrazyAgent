import { listSkills, type Result } from "../../api";
import { agentOf, loadAgentInstructions, processMessage, textBytes, type AgentInstructions as Received, type SkillEntry } from "../../api-process";
import type { GraphNode } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { SkillTag, TaggedText, byteSize, catalogOf } from "../process";

const FILES: Record<string, string> = { codex: "AGENTS.md", "claude-code": "CLAUDE.md" };

function Given({ value, skills }: { value: Received; skills: readonly SkillEntry[] }) {
  if (!value.launched) return <p className="tg-instructions-note">This agent hasn't launched yet. What it receives shows here after its first launch.</p>;
  const known = new Map<string, SkillEntry>();
  for (const skill of skills) if (!known.has(skill.name)) known.set(skill.name, skill);
  return <>
    <p className="tg-instructions-note"><span className="tg-instructions-file">{FILES[value.harness] ?? "Instructions file"}</span>
      {value.text ? ` ${byteSize(textBytes(value.text))}` : ""}</p>
    {value.text ? <TaggedText text={value.text} skills={skills} />
      : <p className="tg-instructions-note">No app or project instructions were given at its last launch.</p>}
    <h4 className="tg-instructions-title">Skills</h4>
    {value.skills.length > 0 ? <ul className="tg-instructions-skills" aria-label="Skills installed at its last launch">{value.skills.map((name) => {
      const skill = known.get(name);
      return <li key={name}>{skill ? <SkillTag skill={skill} /> : <span>{name}</span>}</li>;
    })}</ul> : <p className="tg-instructions-note">No skills were installed at its last launch.</p>}
  </>;
}

/** The read-only Instructions view of one agent: the instruction file its last launch wrote,
 * with its size and catalog skill names as tags, then the skills installed for it.
 * Inputs: the engine read and the skill catalog. Output: the section. */
export function AgentInstructionsView({ read, skills }: { read: Loaded<Received>; skills: readonly SkillEntry[] }) {
  return <section className="tg-instructions" aria-label="What this agent received">
    <h3 className="tg-instructions-title">Instructions</h3>
    {read.kind === "loading" && <p className="tg-instructions-note" role="status">Reading what this agent received...</p>}
    {read.kind !== "loading" && read.kind !== "ok" && <p className="tg-instructions-note" role="alert">{processMessage(read)}</p>}
    {read.kind === "ok" && <Given value={read.value} skills={skills} />}
  </section>;
}

const NO_AGENT: Result<Received> = { kind: "error", code: "process_agent_unknown", message: "" };

/** The Instructions view for the open graph card: reads what that agent received at its last
 * launch and the project's skill catalog. Inputs: the project id and the card's node.
 * Output: the view, or nothing for a card without an agent. */
export function AgentInstructions({ projectId, node }: { projectId: string; node: GraphNode }) {
  const agent = agentOf(node);
  const [read] = useLoad(() => agent === undefined ? Promise.resolve(NO_AGENT) : loadAgentInstructions(projectId, agent), [projectId, node.id]);
  const [catalog] = useLoad(() => listSkills(projectId), [projectId]);
  if (agent === undefined) return null;
  return <AgentInstructionsView read={read} skills={catalogOf(catalog)} />;
}
