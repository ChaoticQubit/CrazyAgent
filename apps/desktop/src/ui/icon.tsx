import { createElement } from "react";
import iconArrowLeft from "../assets/icons/arrow-left.svg?raw";
import iconArrowRight from "../assets/icons/arrow-right.svg?raw";
import iconBookOpen from "../assets/icons/book-open.svg?raw";
import iconBot from "../assets/icons/bot.svg?raw";
import iconBoxes from "../assets/icons/boxes.svg?raw";
import iconBrain from "../assets/icons/brain.svg?raw";
import iconCheck from "../assets/icons/check.svg?raw";
import iconChevronDown from "../assets/icons/chevron-down.svg?raw";
import iconChevronLeft from "../assets/icons/chevron-left.svg?raw";
import iconChevronRight from "../assets/icons/chevron-right.svg?raw";
import iconCircleAlert from "../assets/icons/circle-alert.svg?raw";
import iconCircleCheck from "../assets/icons/circle-check.svg?raw";
import iconCircleDashed from "../assets/icons/circle-dashed.svg?raw";
import iconCirclePause from "../assets/icons/circle-pause.svg?raw";
import iconCircleX from "../assets/icons/circle-x.svg?raw";
import iconClock from "../assets/icons/clock.svg?raw";
import iconCopy from "../assets/icons/copy.svg?raw";
import iconCpu from "../assets/icons/cpu.svg?raw";
import iconEllipsis from "../assets/icons/ellipsis.svg?raw";
import iconExternalLink from "../assets/icons/external-link.svg?raw";
import iconFile from "../assets/icons/file.svg?raw";
import iconFileCode from "../assets/icons/file-code.svg?raw";
import iconFileImage from "../assets/icons/file-image.svg?raw";
import iconFileSpreadsheet from "../assets/icons/file-spreadsheet.svg?raw";
import iconFileText from "../assets/icons/file-text.svg?raw";
import iconFolder from "../assets/icons/folder.svg?raw";
import iconFolderOpen from "../assets/icons/folder-open.svg?raw";
import iconFolderPlus from "../assets/icons/folder-plus.svg?raw";
import iconFunnel from "../assets/icons/funnel.svg?raw";
import iconGlobe from "../assets/icons/globe.svg?raw";
import iconHardDrive from "../assets/icons/hard-drive.svg?raw";
import iconHouse from "../assets/icons/house.svg?raw";
import iconInbox from "../assets/icons/inbox.svg?raw";
import iconInfo from "../assets/icons/info.svg?raw";
import iconKeyRound from "../assets/icons/key-round.svg?raw";
import iconLayers from "../assets/icons/layers.svg?raw";
import iconLayoutTemplate from "../assets/icons/layout-template.svg?raw";
import iconLink from "../assets/icons/link.svg?raw";
import iconListChecks from "../assets/icons/list-checks.svg?raw";
import iconLoaderCircle from "../assets/icons/loader-circle.svg?raw";
import iconLock from "../assets/icons/lock.svg?raw";
import iconLogIn from "../assets/icons/log-in.svg?raw";
import iconLogOut from "../assets/icons/log-out.svg?raw";
import iconMessageSquare from "../assets/icons/message-square.svg?raw";
import iconMinus from "../assets/icons/minus.svg?raw";
import iconMonitor from "../assets/icons/monitor.svg?raw";
import iconMoon from "../assets/icons/moon.svg?raw";
import iconPalette from "../assets/icons/palette.svg?raw";
import iconPencil from "../assets/icons/pencil.svg?raw";
import iconPlug from "../assets/icons/plug.svg?raw";
import iconPlus from "../assets/icons/plus.svg?raw";
import iconPuzzle from "../assets/icons/puzzle.svg?raw";
import iconRefreshCw from "../assets/icons/refresh-cw.svg?raw";
import iconSearch from "../assets/icons/search.svg?raw";
import iconSend from "../assets/icons/send.svg?raw";
import iconServer from "../assets/icons/server.svg?raw";
import iconSettings from "../assets/icons/settings.svg?raw";
import iconShieldCheck from "../assets/icons/shield-check.svg?raw";
import iconSparkles from "../assets/icons/sparkles.svg?raw";
import iconSquare from "../assets/icons/square.svg?raw";
import iconSquareTerminal from "../assets/icons/square-terminal.svg?raw";
import iconSun from "../assets/icons/sun.svg?raw";
import iconSwords from "../assets/icons/swords.svg?raw";
import iconTerminal from "../assets/icons/terminal.svg?raw";
import iconTrash from "../assets/icons/trash.svg?raw";
import iconTrendingUp from "../assets/icons/trending-up.svg?raw";
import iconTriangleAlert from "../assets/icons/triangle-alert.svg?raw";
import iconUpload from "../assets/icons/upload.svg?raw";
import iconUserRound from "../assets/icons/user-round.svg?raw";
import iconUsers from "../assets/icons/users.svg?raw";
import iconWifiOff from "../assets/icons/wifi-off.svg?raw";
import iconX from "../assets/icons/x.svg?raw";
import iconZap from "../assets/icons/zap.svg?raw";
import logoAnthropic from "../assets/logos/anthropic.svg?raw";
import logoClaude from "../assets/logos/claude.svg?raw";
import logoCursor from "../assets/logos/cursor.svg?raw";
import logoGithubcopilot from "../assets/logos/githubcopilot.svg?raw";
import logoGoogle from "../assets/logos/google.svg?raw";
import logoGooglegemini from "../assets/logos/googlegemini.svg?raw";
import logoOllama from "../assets/logos/ollama.svg?raw";
import logoOpencode from "../assets/logos/opencode.svg?raw";
import logoOpenrouter from "../assets/logos/openrouter.svg?raw";
import logoQwen from "../assets/logos/qwen.svg?raw";
import logoVercel from "../assets/logos/vercel.svg?raw";

const ICON_SOURCES = {
  "arrow-left": iconArrowLeft,
  "arrow-right": iconArrowRight,
  "book-open": iconBookOpen,
  "bot": iconBot,
  "boxes": iconBoxes,
  "brain": iconBrain,
  "check": iconCheck,
  "chevron-down": iconChevronDown,
  "chevron-left": iconChevronLeft,
  "chevron-right": iconChevronRight,
  "circle-alert": iconCircleAlert,
  "circle-check": iconCircleCheck,
  "circle-dashed": iconCircleDashed,
  "circle-pause": iconCirclePause,
  "circle-x": iconCircleX,
  "clock": iconClock,
  "copy": iconCopy,
  "cpu": iconCpu,
  "ellipsis": iconEllipsis,
  "external-link": iconExternalLink,
  "file": iconFile,
  "file-code": iconFileCode,
  "file-image": iconFileImage,
  "file-spreadsheet": iconFileSpreadsheet,
  "file-text": iconFileText,
  "folder": iconFolder,
  "folder-open": iconFolderOpen,
  "folder-plus": iconFolderPlus,
  "funnel": iconFunnel,
  "globe": iconGlobe,
  "hard-drive": iconHardDrive,
  "house": iconHouse,
  "inbox": iconInbox,
  "info": iconInfo,
  "key-round": iconKeyRound,
  "layers": iconLayers,
  "layout-template": iconLayoutTemplate,
  "link": iconLink,
  "list-checks": iconListChecks,
  "loader-circle": iconLoaderCircle,
  "lock": iconLock,
  "log-in": iconLogIn,
  "log-out": iconLogOut,
  "message-square": iconMessageSquare,
  "minus": iconMinus,
  "monitor": iconMonitor,
  "moon": iconMoon,
  "palette": iconPalette,
  "pencil": iconPencil,
  "plug": iconPlug,
  "plus": iconPlus,
  "puzzle": iconPuzzle,
  "refresh-cw": iconRefreshCw,
  "search": iconSearch,
  "send": iconSend,
  "server": iconServer,
  "settings": iconSettings,
  "shield-check": iconShieldCheck,
  "sparkles": iconSparkles,
  "square": iconSquare,
  "square-terminal": iconSquareTerminal,
  "sun": iconSun,
  "swords": iconSwords,
  "terminal": iconTerminal,
  "trash": iconTrash,
  "trending-up": iconTrendingUp,
  "triangle-alert": iconTriangleAlert,
  "upload": iconUpload,
  "user-round": iconUserRound,
  "users": iconUsers,
  "wifi-off": iconWifiOff,
  "x": iconX,
  "zap": iconZap,
} as const;

const LOGO_SOURCES: Record<string, string> = {
  "anthropic": logoAnthropic,
  "claude": logoClaude,
  "cursor": logoCursor,
  "githubcopilot": logoGithubcopilot,
  "google": logoGoogle,
  "googlegemini": logoGooglegemini,
  "ollama": logoOllama,
  "opencode": logoOpencode,
  "openrouter": logoOpenrouter,
  "qwen": logoQwen,
  "vercel": logoVercel,
};

const SHAPES = new Set(["path", "circle", "rect", "line", "polyline", "polygon", "ellipse"]);
const SHAPE_ATTRIBUTES = new Set(["d", "cx", "cy", "r", "rx", "ry", "x", "y", "x1", "x2", "y1", "y2", "width", "height", "points"]);
const SVG_ATTRIBUTES = new Set(["xmlns", "width", "height", "viewBox", "fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin", "class", "role"]);

const LOGO_FOR: Record<string, string> = {
  claude: "claude",
  anthropic_api: "anthropic",
  openrouter: "openrouter",
  "claude-code": "claude",
  opencode: "opencode",
  "gemini-cli": "googlegemini",
  "cursor-agent": "cursor",
  fx: "vercel",
  antigravity: "google",
  "qwen-code": "qwen",
  copilot: "githubcopilot",
  ollama: "ollama",
};

const MONOGRAM_FOR: Record<string, string> = {
  chatgpt: "CG",
  openai_api: "OA",
  codex: "CX",
  openclaw: "OC",
  droid: "FD",
};

const ICON_FOR: Record<string, IconName> = {
  openai_compatible: "link",
  local: "cpu",
};

export type IconName = keyof typeof ICON_SOURCES;
export const ICON_NAMES = Object.keys(ICON_SOURCES) as IconName[];

export interface SvgShape {
  viewBox: string;
  children: { tag: string; attrs: Record<string, string> }[];
}

function attributes(text: string): Record<string, string> | undefined {
  const result: Record<string, string> = {};
  const pattern = /^([a-zA-Z][a-zA-Z0-9-]*)="([^"<>]*)"\s*/;
  let rest = text.trim();
  while (rest) {
    const match = pattern.exec(rest);
    if (!match || match[1] in result) return undefined;
    result[match[1]] = match[2];
    rest = rest.slice(match[0].length);
  }
  return result;
}

/** Parses one vendored 24 by 24 SVG file into plain shape descriptions, refusing anything
 * that is not a basic shape with geometry attributes (no scripts, links, styles or images).
 * Input: the SVG source text. Output: the view box and its shapes, or an error. */
export function parseSvg(source: string): SvgShape {
  const rejected = new Error("icon_svg_rejected");
  const open = /^<svg\b([^<>]*)>([\s\S]*)<\/svg>$/.exec(source.replace(/<title>[^<>]*<\/title>/g, "").trim());
  if (!open) throw rejected;
  const root = attributes(open[1]);
  if (!root || root.viewBox !== "0 0 24 24" || Object.keys(root).some((name) => !SVG_ATTRIBUTES.has(name))) throw rejected;
  const children: SvgShape["children"] = [];
  const element = /^<([a-z]+)\b([^<>]*?)\/>\s*/;
  let rest = open[2].trim();
  while (rest) {
    const match = element.exec(rest);
    const attrs = match ? attributes(match[2]) : undefined;
    if (!match || !SHAPES.has(match[1]) || !attrs || Object.entries(attrs).some(([name, value]) =>
      !SHAPE_ATTRIBUTES.has(name) && !(name === "fill" && (value === "currentColor" || value === "none")))) throw rejected;
    children.push({ tag: match[1], attrs });
    rest = rest.slice(match[0].length);
  }
  if (children.length === 0) throw rejected;
  return { viewBox: root.viewBox, children };
}

const parsed = new Map<string, SvgShape>();

function shape(key: string, source: string): SvgShape {
  let value = parsed.get(key);
  if (!value) {
    value = parseSvg(source);
    parsed.set(key, value);
  }
  return value;
}

function draw(value: SvgShape) {
  return value.children.map((child, index) => createElement(child.tag, { key: index, ...child.attrs }));
}

/** Renders one vendored Lucide line icon as inline SVG in the current text color.
 * Inputs: icon name, pixel size, an optional accessible label (decorative when absent),
 * optional class and spin. Output: an svg element. */
export function Icon({ name, size = 16, label, className, spin = false }: {
  name: IconName;
  size?: number;
  label?: string;
  className?: string;
  spin?: boolean;
}) {
  const value = shape(name, ICON_SOURCES[name]);
  return <svg className={["icon", spin ? "spin" : "", className ?? ""].filter(Boolean).join(" ")} width={size} height={size}
    viewBox={value.viewBox} fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round"
    focusable="false" aria-hidden={label ? undefined : true} role={label ? "img" : undefined} aria-label={label}>{draw(value)}</svg>;
}

/** Names the CC0 Simple Icons logo used for a provider or harness id, if one exists.
 * Input: a provider or harness id. Output: the logo name or undefined. */
export function providerLogo(id: string): string | undefined {
  const name = LOGO_FOR[id];
  return name && LOGO_SOURCES[name] ? name : undefined;
}

function monogram(id: string, name: string): string {
  const fixed = MONOGRAM_FOR[id];
  if (fixed) return fixed;
  const words = name.replace(/[^\p{L}\p{N} ]/gu, " ").trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? words[0][0] + words[1][0] : (words[0] ?? id).slice(0, 2);
  return letters.toUpperCase();
}

/** Identifies a provider or harness with its monochrome logo, a line icon or a two-letter
 * monogram, in neutral app colors. Inputs: the provider or harness id, its display name and
 * an optional large size. Output: a decorative badge with the name as its tooltip. */
export function ProviderMark({ provider, name, large = false }: { provider: string; name: string; large?: boolean }) {
  const logo = providerLogo(provider);
  const icon = ICON_FOR[provider];
  const size = large ? 18 : 15;
  return <span className={`provider-mark${large ? " large" : ""}`} title={name} aria-hidden="true">
    {logo ? <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" focusable="false">{draw(shape(`logo:${logo}`, LOGO_SOURCES[logo]))}</svg>
      : icon ? <Icon name={icon} size={size} /> : monogram(provider, name)}
  </span>;
}
