import { isMac, isWindows } from "../../ui/keys";

export type RendererChoice = "gpu" | "auto" | "standard";
export const RENDERER_CHOICES: { value: RendererChoice; label: string }[] = [
  { value: "gpu", label: "GPU" },
  { value: "auto", label: "Automatic" },
  { value: "standard", label: "Standard" },
];

/** WebGL renderers that draw in software or through a virtual machine's graphics: drawing a
 * terminal there costs many times the CPU of DOM rows (hresearch: 245-356 s CPU per minute
 * against 10.6-19.8 for SwiftShader), so no choice draws on them. */
const SOFTWARE_RENDERER = /\b(swiftshader|llvmpipe|softpipe|software rasterizer|software adapter|basic render|virgl|svga3d)\b/i;

/** What a page's machine offers for drawing terminals: whether WebGL2 starts, the vendor and
 * renderer it reports (empty when it reports none) and whether it is Linux under Wayland. */
export type GpuProbe = { webgl2: boolean; identity: string; wayland: boolean };

let probed: GpuProbe | null = null;
let autoFailed = false;

/** The choice a window starts on before the owner picks one: GPU on macOS and Windows, whose
 * webviews report the real renderer, and Standard anywhere else. On Linux, WebKit reports
 * "Apple GPU" even over llvmpipe, so the software check cannot see it there (hresearch
 * NOTE-1215). Input: the platform name, by default the window's. Output: the choice. */
export function rendererDefault(platform: string = typeof navigator === "undefined" ? "" : navigator.platform): RendererChoice {
  return isMac(platform) || isWindows(platform) ? "gpu" : "standard";
}

/** Reads a stored Terminal renderer choice. Inputs: the stored text, if any, and the platform
 * name, by default the window's. Output: the choice; the platform's default for anything
 * else. */
export function rendererChoice(stored: string | null, platform?: string): RendererChoice {
  return RENDERER_CHOICES.find((choice) => choice.value === stored)?.value ?? rendererDefault(platform);
}

/** Says whether a terminal draws on the GPU. Standard never does; no choice does without
 * WebGL2 or on a software or virtual machine renderer; Automatic also keeps DOM rows on Linux
 * under Wayland and once a start on the GPU failed. Inputs: the choice, the machine's probe
 * and whether an Automatic start failed since the choice last changed. Output: the answer. */
export function gpuWanted(choice: RendererChoice, probe: GpuProbe, failed: boolean): boolean {
  if (choice === "standard" || !probe.webgl2 || SOFTWARE_RENDERER.test(probe.identity)) return false;
  return choice === "gpu" || (!probe.wayland && !failed);
}

/** Reads once per page what the machine offers, on a scratch canvas whose context is lost
 * right after. The page cannot tell Wayland yet, so it reads as not Wayland until the app
 * reports its display server (leader 11:26). Output: the probe. */
export function probeGpu(): GpuProbe {
  if (probed) return probed;
  let webgl2 = false;
  let identity = "";
  try {
    const gl = document.createElement("canvas").getContext("webgl2");
    if (gl) {
      webgl2 = true;
      const info = gl.getExtension("WEBGL_debug_renderer_info");
      if (info) identity = `${String(gl.getParameter(info.UNMASKED_VENDOR_WEBGL) ?? "")} ${String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL) ?? "")}`.trim();
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    }
  } catch {
    webgl2 = false;
  }
  probed = { webgl2, identity, wayland: false };
  return probed;
}

/** Whether an Automatic start on the GPU failed since the choice last changed. Output: the
 * answer. */
export function autoStartFailed(): boolean {
  return autoFailed;
}

/** Records a start on the GPU that failed: under Automatic it keeps every terminal on DOM
 * rows until the choice changes. Input: the choice it failed under. Output: none. */
export function gpuStartFailed(choice: RendererChoice): void {
  if (choice === "auto") autoFailed = true;
}

/** Forgets a failed Automatic start, as a new choice does. Output: none. */
export function rendererChosen(): void {
  autoFailed = false;
}
