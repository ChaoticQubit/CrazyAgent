import { createRoot } from "react-dom/client";
import { App } from "./app";
import { loadNativeTerminal } from "./screens/team/native-terminal";
import { TerminalWall } from "./screens/terminals";
import { wallRoute } from "./screens/terminals/pages";
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
const root = element;
void loadNativeTerminal().then(() => {
  createRoot(root).render(<AppErrorBoundary>{wall === null ? <App /> : <TerminalWall projectId={wall.project} page={wall.page} name={wall.name} />}</AppErrorBoundary>);
});
