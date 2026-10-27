import { useEffect, useState } from "react";
import { loadProjectFiles } from "../../api-projects";
import { useLoad } from "../../state";

export const FILES_DEBOUNCE_MS = 150;
export const FILES_QUERY_BYTES_MAX = 256;

const CONTROL = /\p{Cc}/gu;

/** The query one file-list read sends for the console's `@` text: none while no tag is typed,
 * else the text without control characters, cut at a character edge to the engine's 256-byte
 * bound. Input: the tag text after `@`, or null. Output: the query, or null. */
export function filesQuery(query: string | null): string | null {
  if (query === null) return null;
  const encoder = new TextEncoder();
  let kept = "";
  for (const char of query.replace(CONTROL, "")) {
    if (encoder.encode(kept + char).length > FILES_QUERY_BYTES_MAX) break;
    kept += char;
  }
  return kept;
}

/** The project's files for the console's `@` completion, read again FILES_DEBOUNCE_MS after the
 * tag text stops changing. The bare `@` list is cached across mounts; typed queries are not, so
 * typing never pushes other screens' reads out of the shared cache. Keeps the last list while a
 * new query loads; null once a read fails, so the console says it completes agents only.
 * Inputs: the project id and the tag text after `@`, or null when none is typed.
 * Output: at most 200 paths, or null. */
export function useProjectFiles(projectId: string, query: string | null): readonly string[] | null {
  const wanted = filesQuery(query);
  const [asked, setAsked] = useState(wanted);
  useEffect(() => {
    if (wanted === asked) return;
    const timer = window.setTimeout(() => setAsked(wanted), FILES_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [wanted, asked]);
  const [loaded] = useLoad(
    () => asked === null ? Promise.resolve({ kind: "ok" as const, value: [] as string[] }) : loadProjectFiles(projectId, asked),
    [projectId, asked], asked === "" ? "project-files" : undefined);
  const [files, setFiles] = useState<readonly string[] | null>([]);
  useEffect(() => {
    if (loaded.kind === "ok") setFiles(loaded.value);
    else if (loaded.kind !== "loading") setFiles(null);
  }, [loaded]);
  return files;
}
