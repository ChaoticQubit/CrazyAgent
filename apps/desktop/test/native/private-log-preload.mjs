import { isMainThread } from "node:worker_threads";

if (isMainThread) process.umask(0o077);
