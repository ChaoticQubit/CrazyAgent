import assert from "node:assert/strict";

const SCROLLING = ["auto", "scroll", "overlay"];
const WINDOW_BOXES = ["html", "body", "div#root"];
const ROOM_PX = 1;

/** The box a wheel turn over a point scrolls: the deepest box of the chain whose overflow
 * the owner can scroll and that has more than a pixel of room. Input: the chain, top first.
 * Output: its index, -1 when nothing under the point can scroll. */
export function scroller(chain) {
  for (let index = chain.length - 1; index >= 0; index -= 1) {
    if (SCROLLING.includes(chain[index].overflowY) && chain[index].room > ROOM_PX) return index;
  }
  return -1;
}

/** What moved between two reads of a point: the window, then each box both chains share
 * from the top whose offset changed, apart from the kept index. Inputs: the reads before and
 * after and the index allowed to move, -1 for none. Output: the names, top first. */
export function shifted(before, after, keep) {
  assert(Number.isInteger(keep) && keep >= -1, "the kept index is a whole number from -1");
  const out = before.window.x !== after.window.x || before.window.y !== after.window.y ? ["the window"] : [];
  const shared = Math.min(before.chain.length, after.chain.length);
  for (let index = 0; index < shared && before.chain[index].name === after.chain[index].name; index += 1) {
    const [was, now] = [before.chain[index], after.chain[index]];
    if (index !== keep && (was.top !== now.top || was.left !== now.left)) out.push(was.name);
  }
  return out;
}

/** The parts of the window that are scrolled away from its place: the window itself and
 * html, body or the root when their offset is not zero. Input: a read. Output: the names. */
export function windowOff(state) {
  const off = state.window.x !== 0 || state.window.y !== 0 ? ["the window"] : [];
  return off.concat(state.chain.filter((box) => WINDOW_BOXES.includes(box.name) && (box.top !== 0 || box.left !== 0)).map((box) => box.name));
}

/** Reads what a wheel turn at a point could move: the window's scroll and every box from
 * html down to the element at the point, with its overflow, room and offset, and the
 * overscroll style of html and body. Inputs: the point in window pixels. Output: the read,
 * or null when nothing is drawn there. */
export function scrollState(x, y) {
  return browser.execute((x, y) => {
    const name = (node) => {
      const tag = node.tagName.toLowerCase();
      if (node.id) return `${tag}#${node.id}`;
      if (tag === "html" || tag === "body" || !node.classList.length) return tag;
      return `${tag}.${node.classList[0]}`;
    };
    const chain = [];
    for (let node = document.elementFromPoint(x, y); node; node = node.parentElement) {
      chain.unshift({ name: name(node), overflowY: getComputedStyle(node).overflowY, room: node.scrollHeight - node.clientHeight, top: node.scrollTop, left: node.scrollLeft });
    }
    if (!chain.length) return null;
    return {
      window: { x: window.scrollX, y: window.scrollY },
      chain,
      overscroll: [document.documentElement, document.body].map((node) => getComputedStyle(node).overscrollBehaviorY),
    };
  }, x, y);
}

/** The points a window scroll check turns the wheel over: the centers of the sidebar, its
 * foot and the page panel where they are drawn, each with the element the point lands on.
 * Output: [{ name, x, y, over }] in window pixels. */
export function scrollPoints() {
  return browser.execute(() => [["the sidebar", ".sidebar"], ["the sidebar foot", ".sidebar-foot"], ["the page panel", ".main"]].flatMap(([name, selector]) => {
    const node = [...document.querySelectorAll(selector)].find((item) => item.getClientRects().length);
    if (!node) return [];
    const box = node.getBoundingClientRect();
    const left = Math.max(box.left, 0);
    const top = Math.max(box.top, 0);
    const right = Math.min(box.right, window.innerWidth);
    const bottom = Math.min(box.bottom, window.innerHeight);
    if (right - left < 2 || bottom - top < 2) return [];
    const x = Math.round((left + right) / 2);
    const y = Math.round((top + bottom) / 2);
    const over = document.elementFromPoint(x, y);
    return [{ name, x, y, over: over && node.contains(over) ? selector : (over?.tagName.toLowerCase() ?? "nothing") }];
  }));
}

/** Scrolls the box at one index of a point's chain by a step, as the wheel's own scroll
 * would. Inputs: the point, the index and the signed step, 0 to read its room only.
 * Output: the step it took, or null when the point has no box at that index. */
function scrollBox(x, y, index, step) {
  return browser.execute((x, y, index, step) => {
    const nodes = [];
    for (let node = document.elementFromPoint(x, y); node; node = node.parentElement) nodes.unshift(node);
    const box = nodes[index];
    if (!box) return null;
    const by = step !== 0 ? step : box.scrollTop + 1 > box.scrollHeight - box.clientHeight ? -1 : 1;
    if (step !== 0) box.scrollBy({ top: step, behavior: "instant" });
    return by;
  }, x, y, index, step);
}

/** Scrolls the box a wheel turn at a point would scroll by a step, toward the side it has
 * room on, reads the point before and after, and puts the box back. Inputs: the point, the
 * box's index in the point's chain and the step in pixels. Output: { before, after, by },
 * or null when the box is gone. */
export async function panelScroll(x, y, index, step) {
  assert(Number.isInteger(step) && step > 0, "a panel step is a whole, positive pixel count");
  const side = await scrollBox(x, y, index, 0);
  if (side === null) return null;
  const before = await scrollState(x, y);
  const by = await scrollBox(x, y, index, side * step);
  const after = await scrollState(x, y);
  await scrollBox(x, y, index, -by);
  return { before, after, by };
}
