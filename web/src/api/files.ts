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

import { join as joinBase } from "./base.ts";
import type { ErrorEnvelope } from "./types.ts";

// Joined onto the client's base like every other control path (api/base.ts).
// `/files` is mounted on a session daemon's own TCP listener only, so under a
// host these URLs reach the session's socket, which does not serve them — the
// viewer renders that refusal like any other (docs/host.md).
const fileUrl = (path: string): string =>
  joinBase("/files/" + path.split("/").map(encodeURIComponent).join("/"));

/** The change-poke stream for one file — EventSource sends
 * `Accept: text/event-stream`, which is the negotiation. Same-origin, so the
 * session cookie rides along. */
export const fileStream = (path: string): EventSource =>
  new EventSource(fileUrl(path));

/** The change-poke stream for one directory's own entries — what an expanded
 * tree node holds so its listing stays current (docs/web-bind.md). The
 * trailing slash is the directory spelling; `""` is the root. */
export const dirStream = (path: string): EventSource =>
  new EventSource(dirUrl(path));

const dirUrl = (path: string): string =>
  path === "" ? joinBase("/files/") : fileUrl(path) + "/";

/** One entry of a listing. In a recursive listing `name` is the path relative
 * to the directory listed; in a flat one it is a bare name. */
export interface DirEntry {
  name: string;
  kind: "file" | "dir" | "other";
  size: number;
  modified_at: number | null;
  symlink: boolean;
}

export interface Listing {
  /** The listed directory, relative to the root; `""` at the root. */
  path: string;
  recursive: boolean;
  /** A server cap was hit: this is a prefix of the tree, not the whole of it. */
  truncated: boolean;
  entries: DirEntry[];
}

export type ListFetch =
  | { kind: "ok"; listing: Listing }
  | { kind: "refused"; code: ErrorEnvelope["code"]; message: string };

/** List a directory. `depth` above 1 walks below it — the recursive listing
 * the path finder matches against — and the server clamps both it and the
 * result size, reporting `truncated` rather than erroring. */
export async function listDir(path: string, depth = 1): Promise<ListFetch> {
  const query = depth > 1 ? `?recursive=1&depth=${depth}` : "";
  const response = await fetch(dirUrl(path) + query, {
    headers: { accept: "application/json" },
  });
  if (response.ok) {
    return { kind: "ok", listing: (await response.json()) as Listing };
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
