// The /files read surface (docs/web-bind.md): cookie-only GETs of working-
// directory text files, outside /v1. Refusals are values rather than thrown
// errors because the viewer renders them as content — "not a text file",
// "too large to view" — instead of branching away from them. The conditional
// GET answers a change poke: 304 while unchanged, the new body in the same
// round trip when not.
//
// The same URLs content-negotiate: an Accept naming text/event-stream gets a
// stream of change pokes instead of the body. The Accept here is explicit so
// a read is always a read — the server only streams for an explicit ask, but
// the request states which side it wants rather than leaning on the
// negotiation's default.

import type { ErrorEnvelope } from "./types.ts";

const fileUrl = (path: string): string =>
  "/files/" + path.split("/").map(encodeURIComponent).join("/");

/** The change-poke stream for one file — EventSource sends
 * `Accept: text/event-stream`, which is the negotiation. Same-origin, so the
 * session cookie rides along. */
export const fileStream = (path: string): EventSource =>
  new EventSource(fileUrl(path));

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
  const response = await fetch(fileUrl(path), {
    headers: {
      accept: "text/plain",
      ...(etag == null ? {} : { "if-none-match": etag }),
    },
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
