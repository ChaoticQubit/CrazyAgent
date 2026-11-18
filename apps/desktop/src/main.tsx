import { createRoot } from "react-dom/client";
import { App } from "./app";
import { TerminalWall } from "./screens/terminals";
import { wallRoute } from "./screens/terminals/pages";
import { nativeTerminalEnabled } from "./screens/team/native-terminal";
import { loadTerminalKind } from "./screens/team/terminal-kind";
import { applyTheme, mirrorTheme, readTheme } from "./state";
import { AppErrorBoundary } from "./ui/error-boundary";
import { blockWebContextMenu } from "./ui/shell";
import "./style.css";

const wall = wallRoute(window.location.hash);
applyTheme(readTheme());
if (wall === null) mirrorTheme(readTheme());
blockWebContextMenu(document);
const element = document.getElementById("root");
if (element === null) throw new Error("Desktop root element is missing");
void loadTerminalKind(nativeTerminalEnabled).then(() => {
  createRoot(element).render(<AppErrorBoundary>{wall === null ? <App /> : <TerminalWall projectId={wall.project} page={wall.page} name={wall.name} />}</AppErrorBoundary>);
});
