import { runExecutable } from "./host.runtime.ts";

try {
  await runExecutable(process.argv.slice(2));
  process.exit(0);
} catch (error) {
  const code = error instanceof Error && error.message === "host_control_rejected" ? 64 : 70;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, 1000);
    process.stderr.write(code === 64 ? "host_control_rejected\n" : "host_execution_failed\n", () => { clearTimeout(timer); resolve(); });
  });
  process.exit(code);
}
