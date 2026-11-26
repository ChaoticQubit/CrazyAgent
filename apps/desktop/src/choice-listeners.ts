/** The listeners of one choice kept in this app: a save in this window tells them at once,
 * and the storage event tells them of a save in another window. Input: the stored key.
 * Output: `tell`, called after a save, and `listen`, which returns the call that stops it. */
export function choiceListeners(key: string): { tell: () => void; listen: (listener: () => void) => () => void } {
  const heard = new Set<() => void>();
  let watched = false;
  const tell = () => {
    for (const listener of [...heard]) listener();
  };
  return {
    tell,
    listen: (listener) => {
      if (!watched) {
        watched = true;
        window.addEventListener("storage", (event) => {
          if (event.key === key) tell();
        });
      }
      heard.add(listener);
      return () => {
        heard.delete(listener);
      };
    },
  };
}
