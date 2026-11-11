import { createElement, useState } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { SecretRows, readSecrets } from "../../src/screens/settings/plugins.tsx";

let root = null;
let shown = [];

function Rows() {
  const [rows, setRows] = useState([]);
  shown = rows;
  return createElement(SecretRows, { rows, onRows: setRows });
}

export function mount() {
  root?.unmount();
  shown = [];
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(Rows)));
}

export function secrets() {
  return readSecrets(shown);
}
