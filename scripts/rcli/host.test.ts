import { describe, expect, test } from "bun:test";
import { closeSync, constants, createReadStream, createWriteStream, openSync, writeSync } from "node:fs";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { spawnSync } from "node:child_process";
import { finished } from "node:stream/promises";
import {
  assembleClosedController,
  BoundedEventWriter,
  decodeControl,
  encodeEvent,
  readControls,
} from "./host.ts";

const session = "session-a";
const instance = "instance-a";
const encoder = new TextEncoder();

function control(body: Record<string, unknown>) {
  return {
    protocol_version: 1,
    session_id: session,
    request_id: "request-a",
    control: body,
  };
}

function bytes(value: unknown) {
  return encoder.encode(JSON.stringify(value));
}

function frame(body: Uint8Array) {
  const result = Buffer.alloc(4 + body.byteLength);
  result.writeUInt32BE(body.byteLength);
  result.set(body, 4);
  return result;
}

function payload(encoded: Uint8Array) {
  const result = Buffer.from(encoded);
  expect(result.byteLength).toBeGreaterThan(4);
  expect(result.readUInt32BE()).toBe(result.byteLength - 4);
  expect(result.byteLength - 4).toBeLessThanOrEqual(65_536);
  return result.subarray(4).toString("utf8");
}

async function collect(source: AsyncIterable<Uint8Array>) {
  const values = [];
  for await (const value of readControls(source, session)) values.push(value);
  return values;
}

async function ownedFile(run: (path: string) => Promise<void>) {
  const directory = await mkdtemp(join(tmpdir(), "rcli-host-unit-"));
  try {
    await chmod(directory, 0o700);
    await run(join(directory, "stream.bin"));
  } finally {
    await rm(directory, { recursive: true });
  }
}

async function bounded<T>(pending: Promise<T>, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      pending,
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(message)), 1000); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function fifoFile(run: (output: ReturnType<typeof createWriteStream>, closeReader: () => void) => Promise<void>) {
  await ownedFile(async (path) => {
    const created = spawnSync("/usr/bin/mkfifo", ["-m", "600", path], { timeout: 1000 });
    expect(created.error).toBeUndefined();
    expect(created.status).toBe(0);
    console.error(`owned-mkfifo-reaped:${created.pid}:${created.status}`);
    const reader = openSync(path, constants.O_RDONLY | constants.O_NONBLOCK);
    let readerOpen = true;
    const closeReader = () => {
      if (readerOpen) { closeSync(reader); readerOpen = false; }
    };
    let writerFd: number | undefined;
    let output: ReturnType<typeof createWriteStream> | undefined;
    try {
      writerFd = openSync(path, constants.O_WRONLY);
      output = createWriteStream(path, { fd: writerFd, autoClose: true, highWaterMark: 1 });
      writerFd = undefined;
      await run(output, closeReader);
    } finally {
      closeReader();
      if (writerFd !== undefined) closeSync(writerFd);
      if (output) {
        const completion = finished(output, { readable: false }).catch(() => undefined);
        output.destroy();
        await bounded(completion, "fifo_close_timeout");
        expect(output.closed).toBe(true);
      }
    }
  });
}

describe("closed Rust control contract", () => {
  test("accepts the five exact variants and preserves identities", () => {
    const variants = [
      { kind: "prompt", turn_id: "turn-a", text: "hello" },
      { kind: "steer", turn_id: "turn-a", text: "keep scope" },
      { kind: "cancel", turn_id: "turn-a" },
      {
        kind: "answer",
        turn_id: "turn-a",
        prompt_id: "prompt-a",
        answers: ["one", "two"],
      },
      { kind: "shutdown" },
    ];
    for (const variant of variants) {
      const expected = control(variant);
      expect(decodeControl(bytes(expected), session)).toEqual(expected);
    }
  });

  test("bounds UTF-8 bytes and uses Rust whitespace semantics", () => {
    const valid = control({ kind: "prompt", turn_id: "turn-a", text: "é".repeat(8192) });
    expect(decodeControl(bytes(valid), session)).toEqual(valid);
    const invalid = ["", " ", "\u0085", "\u2003", "é".repeat(8193)];
    for (const text of invalid) {
      expect(() => decodeControl(bytes(control({ kind: "prompt", turn_id: "turn-a", text })), session)).toThrow("host_control_rejected");
    }
    for (const answers of [[], Array(9).fill("x"), [""], ["é".repeat(513)]]) {
      expect(() => decodeControl(bytes(control({ kind: "answer", turn_id: "turn-a", prompt_id: "prompt-a", answers })), session)).toThrow("host_control_rejected");
    }
    const boundary = control({ kind: "answer", turn_id: "turn-a", prompt_id: "prompt-a", answers: Array(8).fill("é".repeat(512)) });
    expect(decodeControl(bytes(boundary), session)).toEqual(boundary);
  });

  test("rejects extra authority and sibling fields, including null", () => {
    const shutdown = control({ kind: "shutdown" });
    const invalid = [
      { ...shutdown, protocol_version: 2 },
      { ...shutdown, protocol_version: "1" },
      { ...shutdown, session_id: "other-session" },
      { ...shutdown, request_id: "../escape" },
      { ...shutdown, request_id: "x".repeat(129) },
      { ...shutdown, model: null },
      control({ kind: "shutdown", turn_id: null }),
      control({ kind: "cancel", turn_id: "turn-a", text: null }),
      control({ kind: "prompt", turn_id: "turn-a", text: "hello", attachments: [] }),
      control({ kind: "prompt", turn_id: "turn-a", text: "hello", credentials: "not-a-credential" }),
      control({ kind: "shell", turn_id: "turn-a", text: "unused" }),
      control({ kind: "answer", turn_id: "turn-a", prompt_id: "prompt-a", answers: [null] }),
    ];
    for (const value of invalid) {
      expect(() => decodeControl(bytes(value), session)).toThrow("host_control_rejected");
    }
  });

  test("rejects ambiguous JSON, invalid scalars and oversized bodies precisely", () => {
    const original = JSON.stringify(control({ kind: "shutdown" }));
    const invalid = [
      original.replace('"protocol_version":1', '"protocol_version":1,"protocol_version":1'),
      original.replace('"protocol_version":1', '"protocol_version":1,"protocol_\\u0076ersion":1'),
      original.replace('"request-a"', '"\\ud800"'),
      original.replace('"protocol_version":1', '"protocol_version":1.5'),
      original + "{}",
      "null",
      "[".repeat(9) + "0" + "]".repeat(9),
      "{",
    ];
    for (const value of invalid) {
      expect(() => decodeControl(encoder.encode(value), session)).toThrow("host_control_rejected");
    }
    for (const value of [new Uint8Array(), new Uint8Array([0xff]), new Uint8Array(65_537)]) {
      expect(() => decodeControl(value, session)).toThrow("host_control_rejected");
    }
  });

  test("rejects floating-point version tokens even when JS parses them as one", () => {
    const source = JSON.stringify(control({ kind: "shutdown" }));
    for (const token of ["1.0", "1e0", "1E+0"]) {
      const encoded = encoder.encode(source.replace('"protocol_version":1', `"protocol_version":${token}`));
      expect(() => decodeControl(encoded, session)).toThrow("host_control_rejected");
    }
  });

  test("checks every identifier byte including trailing line terminators", () => {
    for (const invalid of ["id\n", "id\r", "id\u2028", "id\u2029", "", "é", "a..b", "x".repeat(129)]) {
      expect(() => decodeControl(bytes(control({ kind: "cancel", turn_id: invalid })), session)).toThrow("host_control_rejected");
      expect(() => encodeEvent(session, invalid, 1n, { kind: "ready" })).toThrow("host_event_rejected");
    }
  });
});

describe("bounded actual byte streams", () => {
  test("reads fragmented headers and bodies followed by a coalesced frame", async () => {
    const first = control({ kind: "prompt", turn_id: "turn-a", text: "actual bytes" });
    const second = control({ kind: "shutdown" });
    const encoded = frame(bytes(first));
    const stream = Readable.from([
      encoded.subarray(0, 1),
      encoded.subarray(1, 3),
      encoded.subarray(3, 7),
      Buffer.concat([encoded.subarray(7), frame(bytes(second))]),
    ]);
    expect(await collect(stream)).toEqual([first, second]);
    expect(await collect(Readable.from([]))).toEqual([]);
  });

  test("rejects truncation and invalid declared lengths", async () => {
    const complete = frame(bytes(control({ kind: "shutdown" })));
    const oversizedHeader = Buffer.alloc(4);
    oversizedHeader.writeUInt32BE(65_537);
    for (const value of [Buffer.from([0]), complete.subarray(0, complete.length - 1), Buffer.alloc(4), oversizedHeader]) {
      await expect(collect(Readable.from([value]))).rejects.toThrow("host_control_rejected");
    }
  });

  test("consumes actual owned file bytes through the same reader", async () => {
    await ownedFile(async (path) => {
      const expected = control({ kind: "cancel", turn_id: "turn-a" });
      await writeFile(path, frame(bytes(expected)), { mode: 0o600 });
      expect(await collect(createReadStream(path, { highWaterMark: 3 }))).toEqual([expected]);
    });
  });
});

describe("Rust event framing", () => {
  test("encodes validated event envelopes with exact numeric u64", () => {
    const event = { kind: "output", turn_id: "turn-a", text: "é" };
    const encoded = encodeEvent(session, instance, 1n, event);
    expect(JSON.parse(payload(encoded))).toEqual({ protocol_version: 1, session_id: session, instance_id: instance, sequence: 1, event });
    for (const sequence of [9_007_199_254_740_993n, 18_446_744_073_709_551_615n]) {
      const text = payload(encodeEvent(session, instance, sequence, { kind: "ready" }));
      expect(text).toContain(`"sequence":${sequence.toString()}`);
      expect(text).not.toContain(`"sequence":"${sequence.toString()}"`);
    }
  });

  test("accepts input and terminal variants with their actual byte bounds", () => {
    const events = [
      { kind: "ready" },
      { kind: "output", turn_id: "turn-a", text: "é".repeat(8192) },
      { kind: "input", turn_id: "turn-a", prompt_id: "prompt-a", question: "é".repeat(2048), choices: Array(8).fill("é".repeat(512)) },
      ...["succeeded", "cancelled", "failed"].map((status) => ({ kind: "completed", turn_id: "turn-a", status })),
    ];
    for (const event of events) {
      expect(JSON.parse(payload(encodeEvent(session, instance, 1n, event))).event).toEqual(event);
    }
  });

  test("rejects invalid identities, sequence, scalars and event authority", () => {
    for (const sequence of [0n, -1n, 18_446_744_073_709_551_616n]) {
      expect(() => encodeEvent(session, instance, sequence, { kind: "ready" })).toThrow("host_event_rejected");
    }
    expect(() => encodeEvent("../session", instance, 1n, { kind: "ready" })).toThrow("host_event_rejected");
    expect(() => encodeEvent(session, "", 1n, { kind: "ready" })).toThrow("host_event_rejected");
    const invalid = [
      { kind: "ready", turn_id: null },
      { kind: "output", turn_id: "turn-a", text: "" },
      { kind: "output", turn_id: "turn-a", text: "é".repeat(8193) },
      { kind: "output", turn_id: "turn-a", text: "\ud800" },
      { kind: "input", turn_id: "turn-a", prompt_id: "prompt-a", question: "q".repeat(4097), choices: [] },
      { kind: "input", turn_id: "turn-a", prompt_id: "prompt-a", question: "question", choices: Array(9).fill("x") },
      { kind: "input", turn_id: "turn-a", prompt_id: "prompt-a", question: "question", choices: [""] },
      { kind: "completed", turn_id: "turn-a", status: "approved" },
      { kind: "permission", grant: true },
    ];
    for (const event of invalid) {
      expect(() => encodeEvent(session, instance, 1n, event)).toThrow("host_event_rejected");
    }
  });

  test("rejects sparse choices instead of serializing holes as null", () => {
    expect(() => encodeEvent(session, instance, 1n, {
      kind: "input", turn_id: "turn-a", prompt_id: "prompt-a", question: "question", choices: Array(1),
    })).toThrow("host_event_rejected");
  });
});

describe("owned real output stream", () => {
  test("waits for actual file drain and close", async () => {
    await ownedFile(async (path) => {
      const output = createWriteStream(path, { mode: 0o600, highWaterMark: 1 });
      let drains = 0;
      output.on("drain", () => { drains++; });
      const writer = new BoundedEventWriter(output, session, instance);
      await writer.enqueue({ kind: "ready" });
      await writer.close();
      expect(drains).toBeGreaterThan(0);
      expect(output.closed).toBe(true);
      expect(JSON.parse(payload(await readFile(path))).event).toEqual({ kind: "ready" });
    });
  });

  test("fails closed on actual EPIPE from an owned closed read endpoint", async () => {
    await fifoFile(async (output, closeReader) => {
      closeReader();
      let code: string | undefined;
      try {
        writeSync(output.fd!, Uint8Array.of(0));
      } catch (error) {
        code = (error as NodeJS.ErrnoException).code;
      }
      expect(code).toBe("EPIPE");
      const writer = new BoundedEventWriter(output, session, instance);
      await expect(writer.enqueue({ kind: "ready" })).rejects.toThrow("host_output_failed");
      expect(code).toBe("EPIPE");
      await expect(bounded(writer.close(), "writer_close_after_failure_timeout")).rejects.toThrow("host_output_failed");
    });
  });

  test("bounds pending output while an actual owned pipe is not read", async () => {
    await fifoFile(async (output) => {
      const writer = new BoundedEventWriter(output, session, instance);
      await writer.enqueue({ kind: "ready" });
      let settled = 0;
      const pending: Promise<string>[] = [];
      const enqueue = () => {
        pending.push(writer.enqueue({ kind: "output", turn_id: "turn-a", text: "x".repeat(16_384) }).then(
          () => { settled++; return "written"; },
          (error: Error) => { settled++; return error.message; },
        ));
      };
      for (let index = 0; index < 32; index++) enqueue();
      await new Promise((resolve) => setTimeout(resolve, 100));
      expect(settled).toBeLessThan(32);
      for (let index = 0; index < 65; index++) enqueue();
      const outcomes = await bounded(Promise.all(pending), "pending_output_did_not_settle");
      expect(outcomes).toHaveLength(97);
      expect(outcomes.every((outcome) => outcome === "written" || outcome === "host_output_failed")).toBe(true);
      expect(outcomes).toContain("host_output_failed");
      await expect(bounded(writer.close(), "writer_close_after_failure_timeout")).rejects.toThrow("host_output_failed");
    });
  });

  test("writes ordered framed bytes and closes its actual file", async () => {
    await ownedFile(async (path) => {
      const writer = new BoundedEventWriter(createWriteStream(path, { mode: 0o600 }), session, instance);
      await expect(writer.enqueue({ kind: "output", turn_id: "turn-a", text: "before ready" })).rejects.toThrow("host_event_rejected");
      await writer.enqueue({ kind: "ready" });
      await expect(writer.enqueue({ kind: "ready" })).rejects.toThrow("host_event_rejected");
      await writer.enqueue({ kind: "output", turn_id: "turn-a", text: "actual file bytes" });
      await writer.enqueue({ kind: "completed", turn_id: "turn-a", status: "succeeded" });
      await writer.close();
      await writer.close();
      const encoded = await readFile(path);
      const events = [];
      let offset = 0;
      while (offset < encoded.byteLength) {
        expect(encoded.byteLength - offset).toBeGreaterThanOrEqual(4);
        const length = encoded.readUInt32BE(offset);
        expect(length).toBeGreaterThan(0);
        expect(length).toBeLessThanOrEqual(65_536);
        expect(offset + 4 + length).toBeLessThanOrEqual(encoded.byteLength);
        events.push(JSON.parse(encoded.subarray(offset + 4, offset + 4 + length).toString("utf8")));
        offset += 4 + length;
      }
      expect(offset).toBe(encoded.byteLength);
      expect(events.map((event) => event.sequence)).toEqual([1, 2, 3]);
      expect(events.map((event) => event.event)).toEqual([
        { kind: "ready" },
        { kind: "output", turn_id: "turn-a", text: "actual file bytes" },
        { kind: "completed", turn_id: "turn-a", status: "succeeded" },
      ]);
      expect(events.every((event) => event.protocol_version === 1 && event.session_id === session && event.instance_id === instance)).toBe(true);
    });
  });
});

test("declares actual-controller assembly without fabricating launch dependencies", () => {
  expect(typeof assembleClosedController).toBe("function");
});
