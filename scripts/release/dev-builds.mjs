/** Stub before the dev build checks exist. Output: no build found. */
export function findBuild() {
  return { state: "none" };
}

/** Stub before the dev build checks exist. Output: no release id. */
export function checkBuild() {
  return 0;
}

/** Stub before the dev build checks exist. Output: always move. */
export function pointerDecision() {
  return { move: true };
}

/** Stub before the dev build checks exist. Output: nothing selected. */
export function pruneSelection() {
  return [];
}
