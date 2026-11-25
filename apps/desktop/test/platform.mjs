const NAMES = { mac: "MacIntel", windows: "Win32", linux: "Linux x86_64" };

/** The computers a sentence can name, each with the browser platform name the window reports,
 * the words the app uses for the computer and for its sign-in store. Input: none.
 * Output: one row per macOS, Windows and Linux. */
export const COMPUTERS = [
  { os: "mac", computer: "this Mac", store: "your Mac's keychain" },
  { os: "windows", computer: "this computer", store: "Windows Credential Manager" },
  { os: "linux", computer: "this computer", store: "your system keyring" },
];

/** Runs `run` while the window reports the platform of `os`, then gives back the host's own
 * navigator, also when `run` throws. Inputs: "mac", "windows" or "linux" and a function.
 * Output: what the function returns, awaited. */
export async function onPlatform(os, run) {
  const name = NAMES[os];
  if (name === undefined) throw new Error(`unknown os ${os}`);
  const host = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const hostPlatform = globalThis.navigator?.platform;
  Object.defineProperty(globalThis, "navigator", { value: { platform: name }, configurable: true });
  try {
    return await run();
  } finally {
    if (host) Object.defineProperty(globalThis, "navigator", host);
    else delete globalThis.navigator;
    if (globalThis.navigator?.platform !== hostPlatform) throw new Error("the host's navigator was not given back");
  }
}

/** Loads a source module and every source module under it fresh, as they are on `os`, so the
 * sentences built when a module loads use that computer's words. Inputs: "mac", "windows" or
 * "linux" and the module path from this folder. Output: the module. */
export function importOn(os, path) {
  return onPlatform(os, () => import(`${path}?platform=${os}`));
}

/** Runs `check` once per computer in [`COMPUTERS`], with the module at `path` loaded fresh as
 * it is on that computer and the window reporting that platform while `check` runs.
 * Inputs: the module path from this folder and a check taking the module and the computer row.
 * Output: none; a failed check throws. */
export async function eachComputer(path, check) {
  for (const row of COMPUTERS) {
    const module = await importOn(row.os, path);
    await onPlatform(row.os, () => check(module, row));
  }
}

/** The computer row for the system these tests run on, for rows that render the host's own
 * window. Input: none. Output: the row from [`COMPUTERS`]. */
export const HOST = COMPUTERS.find((row) => row.os === ({ darwin: "mac", win32: "windows" }[process.platform] ?? "linux"));
