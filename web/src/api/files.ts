// The /files read surface (docs/web-bind.md): cookie-only GETs of working-
// directory text files, outside /v1. Refusals are values rather than thrown
// errors because the viewer renders them as content — "not a text file",
// "too large to view" — instead of branching away from them. The conditional
// GET is the viewer's poll: 304 while unchanged, the new body in the same
// round trip when not.

import type { ErrorEnvelope } from "./types.ts";

export type FileFetch =
  | { kind: "ok"; text: string; etag: string | null }
  /** The `If-None-Match` matched — the common poll answer, no body. */
  | { kind: "unchanged" }
  | { kind: "refused"; code: ErrorEnvelope["code"]; message: string };

/** `path` is relative to the daemon's working directory, which is the only
 * root the surface serves (docs/web-bind.md); segments are encoded here so a
 * `#` or `?` in a filename cannot re-enter URL parsing. */
export async function fetchFile(
  path: string,
  etag?: string | null,
): Promise<FileFetch> {
  const url = "/files/" + path.split("/").map(encodeURIComponent).join("/");
  const response = await fetch(url, {
    headers: etag == null ? {} : { "if-none-match": etag },
  });
  if (response.status === 304) return { kind: "unchanged" };
  if (response.ok) {
    return {
      kind: "ok",
      text: await response.text(),
      etag: response.headers.get("etag"),
    };
  }
  try {
    const envelope = (await response.json()) as ErrorEnvelope;
    return { kind: "refused", code: envelope.code, message: envelope.message };
  } catch {
    return {
      kind: "refused",
      code: "internal",
      message: `http ${response.status}`,
    };
  }
}
