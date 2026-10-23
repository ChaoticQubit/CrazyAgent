import { createRoot } from "react-dom/client";
import { App } from "./app";
import { applyTheme, readTheme } from "./state";
import "./style.css";

applyTheme(readTheme());
const element = document.getElementById("root");
if (element === null) throw new Error("Desktop root element is missing");
createRoot(element).render(<App />);
