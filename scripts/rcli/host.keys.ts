import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { constants, type BigIntStats } from "node:fs";
import { lstat, open, realpath, type FileHandle } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import type { Readable, Writable } from "node:stream";
import { setProviderKey } from "../../.local-state/tools/rcli-source/src/core/keys/ProviderKeyStore.ts";
import { identifier } from "./host.codec.ts";

export type PrivateKeyWriteBinding = {
  protocol_version: 1;
  request_id: string;
  generation: number;
  provider: string;
  home_dir: string;
  home_device: string;
  home_inode: string;
  backboard_device: string;
  backboard_inode: string;
  config_device: string;
  config_inode: string;
  config_sha256: string;
};

type Held = {
  file: FileHandle;
  path: string;
  directory: boolean;
  device: string;
  inode: string;
};

const PROFILE = "rcli_private_key_profile_rejected";
const INPUT = "rcli_private_key_input_rejected";
const CANCELLED = "rcli_private_key_cancelled";
const WRITE = "rcli_private_key_write_failed";
const EXISTS = "rcli_private_key_already_configured";
const LIMIT = 8192;
const FIELDS = ["protocol_version", "request_id", "generation", "provider", "home_dir",
  "home_device", "home_inode", "backboard_device", "backboard_inode", "config_device",
  "config_inode", "config_sha256"].sort();

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function binding(value: unknown): PrivateKeyWriteBinding {
  try {
    if (!record(value) || Object.keys(value).sort().join("\0") !== FIELDS.join("\0") ||
        value.protocol_version !== 1 || !Number.isSafeInteger(value.generation) ||
        (value.generation as number) < 1 || typeof value.home_dir !== "string" ||
        !isAbsolute(value.home_dir) || /[\0\r\n]/.test(value.home_dir) ||
        typeof value.config_sha256 !== "string" || !/^[a-f0-9]{64}$/.test(value.config_sha256)) {
      throw new Error(PROFILE);
    }
    identifier(value.request_id);
    identifier(value.provider);
    for (const field of ["home_device", "home_inode", "backboard_device", "backboard_inode",
      "config_device", "config_inode"]) {
      if (typeof value[field] !== "string" || !/^(0|[1-9][0-9]{0,19})$/.test(value[field] as string)) {
        throw new Error(PROFILE);
      }
    }
    return Object.freeze({ ...value }) as PrivateKeyWriteBinding;
  } catch {
    throw new Error(PROFILE);
  }
}

function checkAbort(signal: AbortSignal): void {
  if (signal.aborted) throw new Error(CANCELLED);
}

function privateStat(stat: BigIntStats, directory: boolean): void {
  if ((directory ? !stat.isDirectory() : !stat.isFile()) ||
      stat.uid !== BigInt(process.getuid!()) ||
      (stat.mode & 0o7777n) !== (directory ? 0o700n : 0o600n) ||
      (!directory && stat.nlink !== 1n)) throw new Error(PROFILE);
}

async function retain(path: string, directory: boolean, device: string, inode: string): Promise<Held> {
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW |
    (directory ? constants.O_DIRECTORY : 0));
  try {
    const held = { file, path, directory, device, inode };
    await checkHeld(held);
    return held;
  } catch (error) {
    await file.close();
    throw error;
  }
}

async function checkHeld(held: Held): Promise<void> {
  for (const stat of [await held.file.stat({ bigint: true }), await lstat(held.path, { bigint: true })]) {
    privateStat(stat, held.directory);
    if (stat.dev.toString() !== held.device || stat.ino.toString() !== held.inode) {
      throw new Error(PROFILE);
    }
  }
}

async function bytes(file: FileHandle, limit: number): Promise<Buffer> {
  const before = await file.stat({ bigint: true });
  if (before.size < 1n || before.size > BigInt(limit)) throw new Error(PROFILE);
  const value = Buffer.alloc(Number(before.size));
  let used = 0;
  while (used < value.length) {
    const { bytesRead } = await file.read(value, used, value.length - used, used);
    if (bytesRead === 0) throw new Error(PROFILE);
    used += bytesRead;
  }
  const after = await file.stat({ bigint: true });
  if (after.size !== before.size || after.mtimeNs !== before.mtimeNs || after.ctimeNs !== before.ctimeNs) {
    throw new Error(PROFILE);
  }
  return value;
}

async function checkProfile(value: PrivateKeyWriteBinding, held: Held[]): Promise<void> {
  if (await realpath(value.home_dir) !== value.home_dir) throw new Error(PROFILE);
  for (const item of held) await checkHeld(item);
  const config = await bytes(held[2].file, 65536);
  if (createHash("sha256").update(config).digest("hex") !== value.config_sha256) throw new Error(PROFILE);
  const parsed: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(config));
  if (!record(parsed) || !record(parsed.model) || parsed.model.provider !== value.provider ||
      !Array.isArray(parsed.providers)) throw new Error(PROFILE);
  const matches = parsed.providers.filter((provider: unknown) => record(provider) && provider.id === value.provider);
  if (matches.length !== 1 || !record(matches[0]) || !record(matches[0].auth) ||
      matches[0].auth.type !== "apiKey") throw new Error(PROFILE);
}

async function absentKeys(path: string): Promise<void> {
  let stat: BigIntStats;
  try {
    stat = await lstat(path, { bigint: true });
  } catch (error) {
    if (record(error) && error.code === "ENOENT") return;
    throw new Error(PROFILE);
  }
  privateStat(stat, false);
  throw new Error(EXISTS);
}

async function secret(input: Readable, signal: AbortSignal): Promise<string> {
  checkAbort(signal);
  const frame = Buffer.alloc(LIMIT + 4);
  let used = 0;
  let expected: number | undefined;
  const abort = () => input.destroy(new Error(CANCELLED));
  signal.addEventListener("abort", abort, { once: true });
  try {
    checkAbort(signal);
    for await (const chunk of input) {
      checkAbort(signal);
      if (!(chunk instanceof Uint8Array) || chunk.byteLength > frame.length - used) throw new Error(INPUT);
      frame.set(chunk, used);
      used += chunk.byteLength;
      if (used >= 4 && expected === undefined) {
        expected = frame.readUInt32BE(0);
        if (expected < 1 || expected > LIMIT) throw new Error(INPUT);
      }
      if (expected !== undefined && used > expected + 4) throw new Error(INPUT);
    }
    checkAbort(signal);
    if (expected === undefined || used !== expected + 4) throw new Error(INPUT);
    const value = new TextDecoder("utf-8", { fatal: true }).decode(frame.subarray(4, used));
    if (/[\0\r\n]/.test(value) || value.trim().length === 0) throw new Error(INPUT);
    return value;
  } catch {
    input.destroy();
    throw new Error(signal.aborted ? CANCELLED : INPUT);
  } finally {
    signal.removeEventListener("abort", abort);
    frame.fill(0);
  }
}

async function checkedEncryptedKeys(path: string, provider: string): Promise<void> {
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await file.stat({ bigint: true });
    privateStat(stat, false);
    const parsed: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(await bytes(file, 65536)));
    if (!record(parsed) || parsed.version !== 2 || typeof parsed.salt !== "string" ||
        !record(parsed.keys) || Object.keys(parsed.keys).length !== 1 ||
        !record(parsed.keys[provider])) throw new Error(WRITE);
    const stored = parsed.keys[provider];
    if ("key" in stored || stored.enabled !== true || !record(stored.secret) ||
        Object.keys(stored.secret).sort().join(",") !== "ciphertext,iv,tag" ||
        ![stored.secret.ciphertext, stored.secret.iv, stored.secret.tag].every(item => typeof item === "string" && item.length > 0)) {
      throw new Error(WRITE);
    }
    const named = await lstat(path, { bigint: true });
    privateStat(named, false);
    if (named.dev !== stat.dev || named.ino !== stat.ino) throw new Error(PROFILE);
  } finally {
    await file.close();
  }
}

export async function writePrivateProviderKey(
  metadata: PrivateKeyWriteBinding,
  input: Readable,
  signal: AbortSignal,
): Promise<{ protocol_version: 1; request_id: string; generation: number; provider: string; status: "configured_only" }> {
  const value = binding(metadata);
  checkAbort(signal);
  const held: Held[] = [];
  let writing = false;
  try {
    if (await realpath(value.home_dir) !== value.home_dir) throw new Error(PROFILE);
    held.push(await retain(value.home_dir, true, value.home_device, value.home_inode));
    const backboard = join(value.home_dir, ".backboard");
    held.push(await retain(backboard, true, value.backboard_device, value.backboard_inode));
    held.push(await retain(join(backboard, "config.json"), false, value.config_device, value.config_inode));
    await checkProfile(value, held);
    const keys = join(backboard, "keys.json");
    await absentKeys(keys);
    const key = await secret(input, signal);
    await checkProfile(value, held);
    await absentKeys(keys);
    checkAbort(signal);
    writing = true;
    await setProviderKey(value.provider, key, value.home_dir);
    await checkProfile(value, held);
    await checkedEncryptedKeys(keys, value.provider);
    checkAbort(signal);
    return { protocol_version: 1, request_id: value.request_id, generation: value.generation,
      provider: value.provider, status: "configured_only" };
  } catch (error) {
    const code = error instanceof Error ? error.message : "";
    if ([PROFILE, INPUT, CANCELLED, EXISTS].includes(code)) throw new Error(code);
    throw new Error(writing ? WRITE : PROFILE);
  } finally {
    const closed = await Promise.allSettled(held.map(item => item.file.close()));
    if (closed.some(result => result.status === "rejected")) throw new Error(WRITE);
  }
}

export async function runPrivateProviderKeyAction(
  metadata: string,
  input: Readable,
  output: Writable,
  signal: AbortSignal,
): Promise<void> {
  let value: PrivateKeyWriteBinding;
  try {
    if (typeof metadata !== "string" || Buffer.byteLength(metadata) > LIMIT) throw new Error(PROFILE);
    value = binding(JSON.parse(metadata));
  } catch {
    throw new Error(PROFILE);
  }
  const result = await writePrivateProviderKey(value, input, signal);
  checkAbort(signal);
  const body = Buffer.from(JSON.stringify(result));
  assert(body.byteLength > 0 && body.byteLength <= 4096);
  const frame = Buffer.alloc(body.byteLength + 4);
  frame.writeUInt32BE(body.byteLength, 0);
  frame.set(body, 4);
  const abort = () => output.destroy(new Error(CANCELLED));
  signal.addEventListener("abort", abort, { once: true });
  try {
    checkAbort(signal);
    await new Promise<void>((resolve, reject) => output.write(frame, error => error ? reject(new Error(WRITE)) : resolve()));
  } catch {
    throw new Error(signal.aborted ? CANCELLED : WRITE);
  } finally {
    signal.removeEventListener("abort", abort);
  }
}
