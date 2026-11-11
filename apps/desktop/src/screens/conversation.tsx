const HIDDEN = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u00ad\u034f\u061c\u115f\u1160\u17b4\u17b5\u180b-\u180f\u200b-\u200f\u2028-\u202e\u2060-\u206f\u3164\ufe00-\ufe0f\ufeff\uffa0\ufff0-\ufffb\u{1bca0}-\u{1bca3}\u{1d173}-\u{1d17a}\u{e0000}-\u{e0fff}]/u;
const EMOJI_BASE = /^[\u00a9\u00ae\u203c\u2049\u2122\u2139\u2194-\u21aa\u231a-\u23ff\u24c2\u25aa-\u27bf\u2934\u2935\u2b05-\u2b55\u3030\u303d\u3297\u3299\u{1f000}-\u{1faff}]$/u;
const SELECTOR = /^[\ufe00-\ufe0f]$/u;

function emojiPart(chars: string[], at: number): boolean {
  const before = chars[at - 1] ?? "";
  const after = chars[at + 1] ?? "";
  if (chars[at] === "\ufe0e" || chars[at] === "\ufe0f") return EMOJI_BASE.test(before) && !SELECTOR.test(after);
  if (chars[at] !== "\u200d") return false;
  const base = before === "\ufe0f" ? chars[at - 2] ?? "" : before;
  return EMOJI_BASE.test(base) && EMOJI_BASE.test(after);
}

function escaped(value: string): string {
  const point = value.codePointAt(0)!;
  return point > 0xffff ? `\\u{${point.toString(16)}}` : `\\u${point.toString(16).padStart(4, "0")}`;
}

/** Makes untrusted text safe to show: control, direction-changing, zero-width, tag and other
 * invisible characters become visible escapes, the same set the engine refuses in memory text.
 * One emoji selector or joiner inside an emoji stays. Input: the text. Output: the shown text. */
export function displayText(text: string): string {
  if (!HIDDEN.test(text)) return text;
  const chars = Array.from(text);
  return chars.map((value, at) => HIDDEN.test(value) && !emojiPart(chars, at) ? escaped(value) : value).join("");
}
