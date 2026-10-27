import { skillHref, type SkillEntry } from "../api-process";
import "./skill-tag.css";

/** The tag catalog from the engine's skill list: each skill's id, name and the engine's
 * one-line description, falling back to the full description where no line is reported.
 * Input: the listed skills. Output: one entry per skill, in list order. */
export function skillEntries(skills: readonly { id: string; name: string; description: string; line?: string }[]): SkillEntry[] {
  return skills.map(({ id, name, description, line }) => ({ id, name, description: line || description }));
}

/** One skill name as a tag: hovering shows its one-line description, clicking opens it at
 * `#/settings/plugins?skill=<id>`, or hands it to `onOpen` when the screen opens it in place.
 * A name the catalog doesn't hold renders as plain text. Inputs: the skill name, the catalog,
 * an optional visible label and the optional in-place opener. */
export function SkillTag({ name, skills, label, onOpen }: {
  name: string;
  skills: readonly SkillEntry[];
  label?: string;
  onOpen?: (skill: SkillEntry) => void;
}) {
  const text = label ?? name;
  const skill = skills.find((entry) => entry.name === name);
  if (!skill) return <>{text}</>;
  return <a className="skill-tag" href={skillHref(skill.id)} title={skill.description || skill.name}
    aria-label={skill.description ? `Skill ${text}: ${skill.description}` : `Skill ${text}`}
    onClick={onOpen ? (event) => { event.preventDefault(); onOpen(skill); } : undefined}>{text}</a>;
}
