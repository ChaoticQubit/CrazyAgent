import { execFileSync, spawnSync } from "node:child_process";
import { setTimeout as pause } from "node:timers/promises";

const MARK = "AboutThisMac event received";
const SETTLE_MS = 2000;

/** The local time the macOS log takes for `--start`, to the second. Input: the date. Output:
 * the text, such as 2026-11-16 13:30:05. */
export function logTime(date) {
  const two = (value) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())} ${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}`;
}

/** Counts the About This Mac windows the login window opened since a time, read from the
 * macOS log without changing anything. Input: the start, as `logTime` writes it. Output: the
 * count. */
export function aboutThisMacSince(start) {
  const text = execFileSync("/usr/bin/log", ["show", "--start", start, "--style", "compact", "--predicate", `process == "loginwindow" AND eventMessage CONTAINS "${MARK}"`], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  return text.split("\n").filter((line) => line.includes(MARK)).length;
}

/** Runs one command and says how many About This Mac windows opened while it ran, so a test
 * run that reaches the Mac menu fails here (ABOUT-THIS-MAC). Input: the command after `--`.
 * Output: one line, `ABOUT-THIS-MAC <count> <command exit code>`; exit 1 when any opened or
 * the command failed. */
async function main() {
  const at = process.argv.indexOf("--");
  const command = at < 0 ? [] : process.argv.slice(at + 1);
  if (process.platform !== "darwin" || command.length === 0) {
    console.error("usage on macOS: node test/about-this-mac.mjs -- <command> [args...]");
    process.exit(2);
  }
  const start = logTime(new Date(Date.now() - 1000));
  const run = spawnSync(command[0], command.slice(1), { stdio: "inherit" });
  await pause(SETTLE_MS);
  const opened = aboutThisMacSince(start);
  console.log(`ABOUT-THIS-MAC ${opened} ${run.status}`);
  process.exit(opened === 0 && run.status === 0 ? 0 : 1);
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
