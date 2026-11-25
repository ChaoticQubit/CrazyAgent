/** Makes the id of a new pool, so the page can select the pool it creates. Output: the id. */
export function newPoolId(): string {
  const bytes = new Uint8Array(10);
  crypto.getRandomValues(bytes);
  return "pool-" + Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
