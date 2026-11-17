import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";

/** The four restyle guards, in report order (FEATURE-250 section 5, "Restyle guards"). */
export const GUARDS = ["color", "accent", "weight600", "fontSize"];

/**
 * Counts what the restyle guards refuse in one CSS text: literal colors, uses of the retired
 * accent tokens, weight 600 in any rule but h1 and h2, and literal font sizes.
 * Input: the CSS text. Output: an object of guard name to count.
 */
export function guardCounts(text) {
  let weight600 = 0;
  for (const [, selector, body] of text.matchAll(/([^{}]*)\{([^{}]*)\}/g)) {
    const heads = selector.split(",").map((head) => head.trim());
    if (heads.every((head) => head === "h1" || head === "h2")) continue;
    weight600 += (body.match(/font-weight:\s*600\b|font:\s*600\b/g) ?? []).length;
  }
  return {
    color: (text.match(/#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/g) ?? []).length,
    accent: (text.match(/var\(--accent/g) ?? []).length,
    weight600,
    fontSize: (text.match(/font-size:\s*[0-9.]+(?:px|rem|em)\b|font:[^;]*\b[0-9.]+px/g) ?? []).length,
  };
}

/**
 * Counts every guard over each desktop stylesheet but tokens.css, the one place colors,
 * sizes and weights are defined. Input: the apps/desktop folder.
 * Output: an object of guard name to an object of `src/...` path to count, zeros included.
 */
export function scan(desktop) {
  const counts = Object.fromEntries(GUARDS.map((guard) => [guard, {}]));
  const files = readdirSync(path.join(desktop, "src"), { recursive: true })
    .map((file) => `src/${String(file).split(path.sep).join("/")}`)
    .filter((file) => file.endsWith(".css") && file !== "src/ui/tokens.css")
    .sort();
  for (const file of files) {
    const found = guardCounts(readFileSync(path.join(desktop, file), "utf8"));
    for (const guard of GUARDS) counts[guard][file] = found[guard];
  }
  return counts;
}

/**
 * Judges counts against the ratchet allowlist, which only shrinks: a file may not exceed its
 * entry or appear without one, and an entry above its count, at zero or for no stylesheet
 * must be lowered or removed. Inputs: `scan` counts and the allowlist of the same shape.
 * Output: one line per problem, sorted; empty when every guard holds.
 */
export function ratchet(counts, allow) {
  const problems = [];
  for (const guard of GUARDS) {
    const allowed = allow[guard] ?? {};
    for (const [file, count] of Object.entries(counts[guard])) {
      const entry = allowed[file];
      if (entry === undefined && count > 0) problems.push(`${guard} ${file}: ${count}, allowed 0; use the tokens`);
    }
    for (const [file, entry] of Object.entries(allowed)) {
      const count = counts[guard][file];
      if (!Number.isInteger(entry) || entry < 1) problems.push(`${guard} ${file}: entry ${JSON.stringify(entry)} is not a count above 0`);
      else if (count === undefined) problems.push(`${guard} ${file}: no such stylesheet; remove its entry`);
      else if (count > entry) problems.push(`${guard} ${file}: ${count}, allowed at most ${entry}; the allowlist only shrinks`);
      else if (count === 0) problems.push(`${guard} ${file}: 0; remove its entry`);
      else if (count < entry) problems.push(`${guard} ${file}: ${count}; lower its entry from ${entry} to ${count}`);
    }
  }
  return problems.sort();
}
