import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

const { RENDERER_CHOICES, RENDERER_DEFAULT, gpuWanted, rendererChoice } = await import("../src/screens/team/terminal-renderer.ts");

const HARDWARE = { webgl2: true, identity: "Google Inc. (Apple) ANGLE (Apple, ANGLE Metal Renderer: Apple M2 Max, Unspecified Version)", wayland: false };

test("#358 r2: Terminal renderer offers GPU, Automatic and Standard, GPU by default, and reads anything else as GPU", () => {
  assert.deepEqual(RENDERER_CHOICES.map((choice) => [choice.value, choice.label]), [["gpu", "GPU"], ["auto", "Automatic"], ["standard", "Standard"]]);
  assert.equal(RENDERER_DEFAULT, "gpu");
  for (const stored of [null, "", "webgl", "GPU", "dom"]) assert.equal(rendererChoice(stored), "gpu", String(stored));
  for (const stored of ["gpu", "auto", "standard"]) assert.equal(rendererChoice(stored), stored);
});

test("#358 r2: which choice draws on the GPU on which machine", () => {
  const software = ["Google Inc. (Google) ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero)), SwiftShader driver)", "Mesa llvmpipe (LLVM 15.0.7, 256 bits)", "Mesa softpipe", "Microsoft Basic Render Driver", "Software Rasterizer", "Software Adapter", "Red Hat virgl", "VMware, Inc. SVGA3D; build: RELEASE"];
  const rows = [
    ["gpu", HARDWARE, false, true],
    ["gpu", HARDWARE, true, true],
    ["gpu", { ...HARDWARE, wayland: true }, false, true],
    ["gpu", { ...HARDWARE, webgl2: false }, false, false],
    ["gpu", { ...HARDWARE, identity: "" }, false, true],
    ["auto", HARDWARE, false, true],
    ["auto", HARDWARE, true, false],
    ["auto", { ...HARDWARE, wayland: true }, false, false],
    ["auto", { ...HARDWARE, webgl2: false }, false, false],
    ["auto", { ...HARDWARE, identity: "" }, false, true],
    ["standard", HARDWARE, false, false],
  ];
  for (const [choice, probe, failed, drawn] of rows) assert.equal(gpuWanted(choice, probe, failed), drawn, JSON.stringify({ choice, probe, failed }));
  for (const identity of software) {
    for (const choice of ["gpu", "auto", "standard"]) assert.equal(gpuWanted(choice, { ...HARDWARE, identity }, false), false, `${choice} on ${identity}`);
  }
  assert.equal(gpuWanted("gpu", { ...HARDWARE, identity: "NVIDIA GeForce RTX 4090 softpipeline" }, false), true, "a word that only starts with softpipe is no software renderer");
});
