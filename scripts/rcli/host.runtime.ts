import assert from "node:assert/strict";
import { lstat, realpath } from "node:fs/promises";
import { isAbsolute, join, normalize } from "node:path";
import { Writable } from "node:stream";
import type { AgentEvent, TurnStatus } from "../../.local-state/tools/rcli-source/src/core/bus/events.ts";
import { BoundedEventWriter, identifier, readControls, type Control, type EventBody } from "./host.codec.ts";
import { SdkTurnProjection, type HostAction } from "./host.turns.ts";

export type Launch = { sessionId: string; instanceId: string; homeDir: string; cwd: string };

export function parseLaunch(argv: string[]): Launch {
  try {
    if (!Array.isArray(argv) || argv.length !== 8 || argv.some((value) => typeof value !== "string")) throw new Error();
    const fields = new Map<string, string>();
    const names = ["--session-id", "--instance-id", "--home-dir", "--cwd"];
    for (let index = 0; index < argv.length; index += 2) {
      const key = argv[index]!;
      if (!names.includes(key) || fields.has(key)) throw new Error();
      fields.set(key, argv[index + 1]!);
    }
    const sessionId = identifier(fields.get("--session-id"));
    const instanceId = identifier(fields.get("--instance-id"));
    const homeDir = fields.get("--home-dir")!;
    const cwd = fields.get("--cwd")!;
    for (const path of [homeDir, cwd]) {
      if (!path.isWellFormed() || path.includes("\0") || Buffer.byteLength(path) > 4096 || !isAbsolute(path) || normalize(path) !== path || path === "/") throw new Error();
    }
    if (homeDir === cwd) throw new Error();
    return { sessionId, instanceId, homeDir, cwd };
  } catch { throw new Error("host_launch_rejected"); }
}

export async function admitLaunch(launch: Launch, environmentHome: string | undefined): Promise<void> {
  try {
    if (environmentHome !== launch.homeDir || launch.cwd === launch.homeDir || typeof process.getuid !== "function") throw new Error();
    for (const path of [launch.homeDir, launch.cwd]) {
      const metadata = await lstat(path);
      if (!metadata.isDirectory() || metadata.isSymbolicLink() || metadata.uid !== process.getuid() || (metadata.mode & 0o077) !== 0 || await realpath(path) !== path) throw new Error();
    }
    const anchor = await lstat(join(launch.cwd, ".git"));
    if (anchor.isSymbolicLink() || anchor.uid !== process.getuid() || !anchor.isDirectory() && !anchor.isFile()) throw new Error();
  } catch { throw new Error("host_launch_rejected"); }
}

export async function within<T>(promise: Promise<T>, milliseconds: number): Promise<T> {
  assert(Number.isSafeInteger(milliseconds) && milliseconds > 0 && milliseconds <= 6000, "host_deadline_invariant");
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error("host_deadline")), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

export class HostChannel {
  private readonly output: Writable;
  private readonly writer: BoundedEventWriter;
  private readonly turns = new SdkTurnProjection();
  private readonly pending = new Set<Promise<void>>();
  private readonly abort = new AbortController();
  private resolveFailure!: (error: Error) => void;
  readonly failure: Promise<Error>;
  private initializing = false;
  private started = false;
  private stopping = false;
  private closed = false;
  private closeTask: Promise<void> | null = null;
  private readonly outputError = () => this.fail("host_output_failed");
  private readonly outputClose = () => { if (!this.closed) this.fail("host_output_failed"); };

  constructor(output: Writable, sessionId: string, instanceId: string) {
    identifier(sessionId);
    identifier(instanceId);
    this.output = output;
    this.writer = new BoundedEventWriter(output, sessionId, instanceId);
    this.failure = new Promise((resolve) => { this.resolveFailure = resolve; });
    output.on("error", this.outputError);
    output.on("close", this.outputClose);
  }

  get signal(): AbortSignal { return this.abort.signal; }

  async ready(): Promise<void> {
    this.check();
    if (this.initializing || this.closed || this.stopping || this.signal.aborted) throw new Error("host_not_ready");
    this.initializing = true;
    await this.send({ kind: "ready" });
    if (this.signal.aborted) throw this.signal.reason;
    this.started = true;
  }

  receive(control: Control): HostAction {
    this.check();
    if (!this.started || this.closed || this.stopping || this.signal.aborted) throw new Error("host_not_ready");
    const action = this.turns.admit(control);
    if (action.kind === "answer") this.publish(action.events);
    return action;
  }

  start(turnId: string): void {
    try {
      this.check();
      if (!this.started || this.closed || this.stopping || this.signal.aborted) throw new Error();
      this.turns.start(turnId);
    } catch {
      this.fail("host_callback_failed");
      throw this.signal.reason;
    }
  }

  observe(event: AgentEvent): void {
    if (this.closed || this.signal.aborted) return;
    try {
      this.check();
      this.publish(this.turns.event(event));
    } catch { this.fail("host_callback_failed"); }
  }

  complete(turnId: string, status: TurnStatus): void {
    if (this.closed || this.signal.aborted) return;
    try {
      this.check();
      this.publish(this.turns.complete(turnId, status));
    } catch { this.fail("host_callback_failed"); }
  }

  stop(): void {
    this.check();
    this.stopping = true;
    if (!this.closed) this.turns.stop();
  }

  fail(code: string): void {
    if (this.signal.aborted) return;
    const safe = ["host_output_failed", "host_callback_failed", "host_turn_failed"].includes(code) ? code : "host_callback_failed";
    const error = new Error(safe);
    this.resolveFailure(error);
    this.abort.abort(error);
    this.output.destroy();
  }

  close(): Promise<void> {
    if (this.closeTask) return this.closeTask;
    this.closed = true;
    this.closeTask = (async () => {
      try {
        await this.writer.close();
        if (this.signal.aborted) throw this.signal.reason;
      } catch {
        if (!this.signal.aborted) this.fail("host_output_failed");
        throw this.signal.reason;
      } finally {
        this.turns.close();
        this.output.off("error", this.outputError);
        this.output.off("close", this.outputClose);
      }
    })();
    return this.closeTask;
  }

  private publish(events: EventBody[]): void {
    for (const event of events) {
      if (this.signal.aborted) return;
      this.send(event);
    }
  }

  private send(event: EventBody): Promise<void> {
    this.check();
    if (this.signal.aborted) throw this.signal.reason;
    if (this.pending.size >= 64) {
      this.fail("host_output_failed");
      throw this.signal.reason;
    }
    const task = this.writer.enqueue(event);
    this.pending.add(task);
    void task.then(() => this.pending.delete(task), () => {
      this.pending.delete(task);
      this.fail("host_output_failed");
    });
    return task;
  }

  private check(): void {
    assert(this.pending.size <= 64 && (!this.started || this.initializing), "host_channel_invariant");
  }
}

export async function runExecutable(argv: string[]): Promise<void> {
  const launch = parseLaunch(argv);
  await within(admitLaunch(launch, process.env.HOME), 5000);
  const [{ Config }, { createAgentClient }, { buildPermissionContext }, { assembleClosedController }] = await within(Promise.all([
    import("../../.local-state/tools/rcli-source/src/config/Config.ts"),
    import("../../.local-state/tools/rcli-source/src/providers/createAgentClient.ts"),
    import("../../.local-state/tools/rcli-source/src/core/permissions/index.ts"),
    import("./host.ts"),
  ]), 5000);
  const config = new Config({ argv: ["--cwd", launch.cwd], homeDir: launch.homeDir });
  if (!config.hasBackendForCurrentModel) throw new Error("host_startup_failed");
  const client = createAgentClient(config);
  const permissions = buildPermissionContext(config.cwd, "manual", false);
  if (permissions.mode !== "manual" || permissions.interactive || permissions.rules.allow.length !== 0) throw new Error("host_startup_failed");
  const { controller, bus } = assembleClosedController({ config, client, permissions, sessionId: launch.sessionId, cwd: launch.cwd, homeDir: launch.homeDir });
  const channel = new HostChannel(process.stdout, launch.sessionId, launch.instanceId);
  const runs = new Set<Promise<void>>();
  let stopping = false;
  let ready = false;
  let failure: Error | null = null;
  const stop = () => {
    if (stopping) return;
    stopping = true;
    try {
      channel.stop();
      controller.cancel({ clearQueue: true });
    } catch { failure ??= new Error("host_cleanup_failed"); }
    finally { process.stdin.destroy(); }
  };
  const detach = bus.onAny((event) => channel.observe(event));
  channel.signal.addEventListener("abort", stop, { once: true });
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  try {
    await within(controller.start(), 5000);
    await within(channel.ready(), 5000);
    ready = true;
    for await (const control of readControls(process.stdin, launch.sessionId)) {
      if (stopping || channel.signal.aborted) break;
      const action = channel.receive(control);
      if (action.kind === "submit") {
        assert(runs.size < 2, "host_inflight_turn_invariant");
        const options = { onStart: () => channel.start(action.turnId) };
        const result = action.steer ? controller.steer(action.text, options) : controller.submit(action.text, options);
        const run = result.then((status) => channel.complete(action.turnId, status), () => channel.fail("host_turn_failed"));
        runs.add(run);
        void run.then(() => runs.delete(run), () => { runs.delete(run); channel.fail("host_turn_failed"); });
      } else if (action.kind === "answer") {
        if (action.delivery) controller.provideInput(action.delivery.id, action.delivery.answers);
      } else if (action.kind === "cancel") controller.cancel({ clearQueue: true });
      else { stop(); break; }
    }
  } catch {
    if (channel.signal.aborted) failure ??= channel.signal.reason;
    else if (!stopping) failure ??= new Error(ready ? "host_control_rejected" : "host_startup_failed");
  } finally {
    stop();
    try { await within(controller.settle(5000), 5500); }
    catch { failure ??= new Error("host_deadline"); }
    if (controller.hasActiveWork) failure ??= new Error("host_work_active");
    try { await within(Promise.allSettled([...runs]), 500); }
    catch { failure ??= new Error("host_deadline"); }
    detach();
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
    channel.signal.removeEventListener("abort", stop);
    try { await within(controller.dispose(), 6000); }
    catch { failure ??= new Error("host_cleanup_failed"); }
    if (controller.hasActiveWork) failure ??= new Error("host_work_active");
    try { await within(channel.close(), 5000); }
    catch { failure ??= channel.signal.aborted ? channel.signal.reason : new Error("host_output_failed"); }
    if (channel.signal.aborted) failure ??= channel.signal.reason;
  }
  if (failure) throw failure;
}
