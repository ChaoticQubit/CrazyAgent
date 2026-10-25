import { createRoot } from "react-dom/client";
import { App } from "./app";
import { applyTheme, readTheme } from "./state";
import { blockWebContextMenu } from "./ui/shell";
import "./style.css";

applyTheme(readTheme());
blockWebContextMenu(document);
const element = document.getElementById("root");
if (element === null) throw new Error("Desktop root element is missing");
createRoot(element).render(<App />);
