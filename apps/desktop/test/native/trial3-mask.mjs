/** The secret shapes no trial evidence file may hold, as RegExp sources so the window can
 * mask its own text with the same list before a screenshot: emails, account ids (UUIDs),
 * Bearer values, API keys, GitHub tokens, JWTs, long hex tokens, device codes and long
 * mixed letter and digit tokens. */
export const SECRET_SOURCES = [
  "[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\\.[A-Za-z]{2,}",
  "\\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\\b",
  "Bearer\\s+[A-Za-z0-9._~+/=-]+",
  "\\bsk-[A-Za-z0-9_-]{16,}",
  "\\b(?:ghp|gho|ghs|ghu|github_pat)_[A-Za-z0-9_]{16,}",
  "\\beyJ[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}(?:\\.[A-Za-z0-9_-]+)?",
  "\\b[0-9a-fA-F]{48,}\\b",
  "\\b[A-Z0-9]{4}-[A-Z0-9]{4}\\b",
  "\\b(?![0-9a-f]{40}\\b)(?=[A-Za-z0-9_-]*\\d)(?=[A-Za-z0-9_-]*[A-Za-z])[A-Za-z0-9_-]{40,}\\b",
];

/** Replaces every secret shape in a text with [masked]. Input: the text. Output: the text
 * that may be written into the trial run folder. */
export function mask(text) {
  return SECRET_SOURCES.map((source) => new RegExp(source, "g")).reduce((out, pattern) => out.replace(pattern, "[masked]"), String(text));
}
