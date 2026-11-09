import { createRoot } from "react-dom/client";
import { App } from "./app";
import { TerminalWall } from "./screens/terminals";
import { wallRoute } from "./screens/terminals/pages";
import { applyTheme, readTheme } from "./state";
import { AppErrorBoundary } from "./ui/error-boundary";
import { blockWebContextMenu } from "./ui/shell";
import "./style.css";

applyTheme(readTheme());
blockWebContextMenu(document);
const element = document.getElementById("root");
if (element === null) throw new Error("Desktop root element is missing");
const wall = wallRoute(window.location.hash);
createRoot(element).render(<AppErrorBoundary>{wall === null ? <App /> : <TerminalWall projectId={wall.project} page={wall.page} name={wall.name} />}</AppErrorBoundary>);
