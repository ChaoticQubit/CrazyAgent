export function mask(text) {
  return String(text);
}

export function assertMasked(html) {
  return html;
}

export async function readTrial() {
  return { kind: "trial", title: "", trials: [], images: [] };
}

export async function readLivePass() {
  return { kind: "live", title: "", groups: [], rows: [] };
}

export async function readRetro() {
  return { kind: "trial", title: "", trials: [], images: [] };
}

export function renderReport() {
  return "";
}
