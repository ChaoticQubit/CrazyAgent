const channels = (color) => color.match(/[\d.]+/g).slice(0, 3).map(Number);
const lit = (value) => { const part = value / 255; return part <= 0.03928 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4; };
const luminance = (color) => { const [red, green, blue] = channels(color).map(lit); return 0.2126 * red + 0.7152 * green + 0.0722 * blue; };
const backdrop = (node) => {
  for (let at = node; at; at = at.parentElement) {
    const color = getComputedStyle(at).backgroundColor;
    if (!/rgba\(.*, 0\)|transparent/u.test(color)) return color;
  }
  return "rgb(255, 255, 255)";
};

/** The contrast ratio of one element's text on its background. Input: the selector. Output:
 * the ratio. */
export function contrast(selector) {
  const node = document.querySelector(selector);
  const [high, low] = [luminance(getComputedStyle(node).color), luminance(backdrop(node))].sort((one, two) => two - one);
  return (high + 0.05) / (low + 0.05);
}

/** The contrast ratio of one element's fill on what is behind it. Input: the selector.
 * Output: the ratio. */
export function markContrast(selector) {
  const node = document.querySelector(selector);
  const [high, low] = [luminance(getComputedStyle(node).backgroundColor), luminance(backdrop(node.parentElement))].sort((one, two) => two - one);
  return (high + 0.05) / (low + 0.05);
}
