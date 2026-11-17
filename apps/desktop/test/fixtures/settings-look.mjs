import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import frame from "../../src/screens/settings/settings.css?inline";
import { SettingsFrame } from "../../src/screens/settings/index.tsx";
import { SettingLine, SettingSection } from "../../src/ui/setting.tsx";

let root = null;

/**
 * Mounts the real Settings frame on the Sandbox tab with the real stylesheets: its grouped nav,
 * the Sandbox page head as sandbox.tsx draws it, and one real settings section that changed,
 * holding a sub-heading and one changed line with help. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, frame].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root" style="width: 1100px; padding: 24px"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(h(SettingsFrame, { tab: "sandbox" },
    h("div", { className: "settings-top" }, h("div", null, h("h2", null, "Sandbox"), h("p", null, "What agents can reach while they work."))),
    h("form", { className: "pj set-groups", "aria-label": "Linux sandbox settings" },
      h(SettingSection, { title: "Network", about: "What a limited-network agent may open.", changed: true },
        h("p", { className: "set-sub" }, "Linux"),
        h(SettingLine, { label: "Linux limited-network sockets", help: "TCP only is the safe choice.", changed: true }, h("span", null, "TCP only")))),
  )));
}

/**
 * The color one token paints in the mounted theme, as the browser computes it.
 * Input: the token name without its dashes. Output: an rgb() string.
 */
function token(name) {
  const probe = document.createElement("span");
  probe.style.color = `var(--${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}

/**
 * Reads the Settings nav, page head and section. Output: the tokens and each part's computed
 * fill, color, size, line height or weight.
 */
export function look() {
  const css = (selector, pseudo) => getComputedStyle(document.querySelector(selector), pseudo);
  const type = (selector) => ({ size: css(selector).fontSize, line: css(selector).lineHeight, weight: css(selector).fontWeight, color: css(selector).color });
  const current = css('.settings-nav-item[aria-current="page"]');
  return {
    tokens: Object.fromEntries(["surface-3", "text", "text-2", "text-3", "primary"].map((name) => [name, token(name)])),
    nav: {
      current: { fill: current.backgroundColor, color: current.color, weight: current.fontWeight },
      other: css(".settings-nav-item:not([aria-current])").fontWeight,
      group: type(".settings-nav-title"),
    },
    top: { title: type(".settings-top h2"), about: css(".settings-top p").fontSize },
    section: {
      title: type(".set-head h3"),
      about: { size: css(".set-head p").fontSize, color: css(".set-head p").color },
      sub: type(".set-sub"),
      label: css(".set-label").fontWeight,
      help: css(".set-help").fontSize,
      changed: { color: css(".set-label .set-changed").color, dot: css(".set-label .set-changed", "::before").backgroundColor },
    },
  };
}
