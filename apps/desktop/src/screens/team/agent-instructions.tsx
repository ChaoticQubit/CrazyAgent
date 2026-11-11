import type { ReactNode } from "react";
import { listSkills, type Result } from "../../api";
import {
  agentOf, fileStateWords, loadAgentInstructions, processMessage, reportedLine, textBytes, type AgentInstructions as Received, type SkillEntry,
} from "../../api-process";
import type { GraphNode } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { TaggedText, byteSize, catalogOf } from "../process";
import { SkillTag } from "../skill-tag";
import { TerminalPanel } from "./terminal-panel";

const FILES: Record<string, string> = { codex: "AGENTS.md", "claude-code": "CLAUDE.md" };

function Given({ value, skills }: { value: Received; skills: readonly SkillEntry[] }) {
  if (!value.launched) return <p className="tg-instructions-note">This agent hasn't launched yet. What it receives shows here after its first launch.</p>;
  return <>
    <p className="tg-instructions-reported">{reportedLine(value)}</p>
    {value.loaded.length > 0 && <ul className="tg-instructions-loaded" aria-label="Instruction files its harness loaded">{value.loaded.map((file) =>
      <li key={`${file.name} ${file.path}`}><span className="tg-instructions-file">{file.name}</span> <span className="tg-instructions-path">{file.path}</span></li>)}</ul>}
    <p className="tg-instructions-note"><span className="tg-instructions-file">{FILES[value.harness] ?? "Instructions file"}</span>
      {value.text ? ` ${byteSize(textBytes(value.text))}` : ""}</p>
    {value.text ? <TaggedText text={value.text} skills={skills} />
      : <p className="tg-instructions-note">No app or project instructions were given at its last launch.</p>}
    <h4 className="tg-instructions-title">Instruction files</h4>
    {value.files.length > 0 ? <ul className="tg-instructions-skills" aria-label="Instruction files found at its last launch">{value.files.map((file) =>
      <li key={file.name}><span className="tg-instructions-file">{file.name}</span>{` ${byteSize(file.bytes)}, ${fileStateWords(file.state)}`}</li>)}</ul>
      : <p className="tg-instructions-note">No instruction files were found at its last launch.</p>}
    <h4 className="tg-instructions-title">Skills</h4>
    {value.skills.length > 0 ? <ul className="tg-instructions-skills" aria-label="Skills installed at its last launch">{value.skills.map((name) =>
      <li key={name}><SkillTag name={name} skills={skills} /></li>)}</ul> : <p className="tg-instructions-note">No skills were installed at its last launch.</p>}
  </>;
}

/** The read-only Instructions view of one agent: what its harness reported loading and
 * invoking, the instruction file its last launch wrote, with its size and catalog skill names
 * as tags, then the skills installed for it.
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

/** The one-line report for an agent's terminal head: what its harness loaded and invoked,
 * once the read is in and the agent launched. Input: the read. Output: the line, or nothing. */
export function headReport(read: Loaded<Received>): string | undefined {
  return read.kind === "ok" && read.value.launched ? reportedLine(read.value) : undefined;
}

/** The open graph card's side: the agent's terminal, the one panel a click on another card
 * replaces, whose head carries Stop and the one-line report of what its harness loaded and
 * invoked, and the Instructions view under it, from one read of what the agent received at
 * its last launch and the project's skill catalog.
 * Inputs: the project id, the card's node, a harness name lookup, the close action and the
 * stop action when the agent can be stopped. Output: the side, with no Instructions view for
 * a card without an agent. */
export function AgentSide({ projectId, node, harness, onClose, onStop, children }: {
  projectId: string;
  node: GraphNode;
  harness?: (id: string) => string;
  onClose: () => void;
  onStop?: () => void;
  children?: ReactNode;
}) {
  const agent = agentOf(node);
  const [read] = useLoad(() => agent === undefined ? Promise.resolve(NO_AGENT) : loadAgentInstructions(projectId, agent), [projectId, node.id]);
  const [catalog] = useLoad(() => listSkills(projectId), [projectId]);
  return <div className="tg-side">
    <TerminalPanel node={node} harness={harness} onClose={onClose} onStop={onStop} swaps reported={agent === undefined ? undefined : headReport(read)} />
    {agent !== undefined && <AgentInstructionsView read={read} skills={catalogOf(catalog)} />}
    {children}
  </div>;
}
