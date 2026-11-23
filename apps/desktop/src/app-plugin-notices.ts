import { useEffect } from "react";
import { changedLine, loadAppPlugins } from "./api-app-plugins";
import type { Route } from "./ui/router";
import type { EngineState } from "./ui/shell";

/** Shows one app notification naming every app plugin that changed since it was trusted,
 * read once each time the engine connects, so the owner hears of it outside the page too.
 * Inputs: the engine state and the app's toast call. Output: none. */
export function useAppPluginNotices(engine: EngineState, showToast: (message: string, open?: Route) => void): void {
  useEffect(() => {
    if (engine !== "running") return;
    let live = true;
    void loadAppPlugins().then((result) => {
      const line = live && result.kind === "ok" ? changedLine(result.value) : null;
      if (line) showToast(line, { name: "settings", tab: "app-plugins" });
    });
    return () => { live = false; };
  }, [engine]);
}
