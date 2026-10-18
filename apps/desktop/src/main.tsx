import { createRoot } from "react-dom/client";
import { App } from "./app";
import "./style.css";

const element = document.getElementById("root");
if (element === null) throw new Error("Desktop root element is missing");
createRoot(element).render(<App />);
