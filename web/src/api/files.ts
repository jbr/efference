// The /files read surface (docs/web-bind.md): GETs of working-directory text
// files, outside /v1 and behind the same bearer. Refusals are values rather
// than thrown errors because the viewer renders them as content — "not a text
// file", "too large to view" — instead of branching away from them. The
// conditional GET answers a change poke: 304 while unchanged, the new body in
// the same round trip when not.
//
// The same URLs content-negotiate: an Accept naming text/event-stream gets a
// stream of change pokes instead of the body. The Accept here is explicit so
// a read is always a read — the server only streams for an explicit ask, but
// the request states which side it wants rather than leaning on the
// negotiation's default. The stream side is a fetch like any other, read with
// the SSE parser stream.ts exports (`changeStream` below).

import { join as joinBase } from "./base.ts";
import { apiFetch } from "./http.ts";
import { readEventStream } from "./sse.ts";
import type { DirListing, ErrorBody } from "./types.ts";

// Joined onto the client's base like every other control path (api/base.ts).
// `/files` is mounted on a web listener only (docs/web-bind.md); a listener
// that does not serve it refuses, and the viewer renders that refusal like any
// other.
const fileUrl = (path: string): string =>
  joinBase("/files/" + path.split("/").map(encodeURIComponent).join("/"));

/** A change-poke stream, held until `close()`. */
export interface ChangeStream {
  close(): void;
}

/** What a change-poke stream reports. Neither carries content: the consumer's
 * answer to both is the conditional GET (or re-list) it would otherwise poll
 * with (docs/web-bind.md). */
export interface ChangeStreamCallbacks {
  /** The stream connected — on the first attempt and again on every
   * reconnect. A change can land between a consumer's read and the server's
   * watch being registered, and while a dropped connection was being
   * re-established, so an open is answered like a poke. */
  onOpen(): void;
  /** One poke. `path` is the root-relative path the server names, with CR and
   * LF percent-encoded. A poke is a hint: the server also sends one when it
   * lost track of what changed. */
  onChange(path: string): void;
}

/**
 * `GET` the URL as an event stream and report its pokes, reconnecting after a
 * refusal, a failed request, a stream that ends or one that goes silent
 * (`readEventStream`). The server frames every poke as `event: change` with
 * the path as `data:`, and idles with `:` comments; `sseData` drops the
 * `event:` field and the comments, so each event it yields is a poke. The URL
 * is fixed when the stream opens, so a reconnect after the page has moved to
 * another session still reaches this one.
 */
function changeStream(
  url: string,
  callbacks: ChangeStreamCallbacks,
): ChangeStream {
  return readEventStream({
    url: () => url,
    onConnect: () => callbacks.onOpen(),
    onEvent: (path) => callbacks.onChange(path),
  });
}

/** The change-poke stream for one file. */
export const fileStream = (
  path: string,
  callbacks: ChangeStreamCallbacks,
): ChangeStream => changeStream(fileUrl(path), callbacks);

/** The change-poke stream for one directory's own entries — what an expanded
 * tree node holds so its listing stays current (docs/web-bind.md). A directory
 * is spelled like any other path; `""` is the root. */
export const dirStream = (
  path: string,
  callbacks: ChangeStreamCallbacks,
): ChangeStream => changeStream(fileUrl(path), callbacks);

export type ListFetch =
  | { kind: "ok"; listing: DirListing }
  | { kind: "refused"; code: ErrorBody["code"]; message: string };

/** List a directory. `depth` above 1 walks below it — the recursive listing
 * the path finder matches against — and the server clamps both it and the
 * result size, reporting `truncated` rather than erroring. */
export async function listDir(path: string, depth = 1): Promise<ListFetch> {
  const query = depth > 1 ? `?recursive=1&depth=${depth}` : "";
  const response = await apiFetch(fileUrl(path) + query, {
    headers: { accept: "application/json" },
  });
  if (response.ok) {
    return { kind: "ok", listing: (await response.json()) as DirListing };
  }
  try {
    const envelope = (await response.json()) as ErrorBody;
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
  | { kind: "refused"; code: ErrorBody["code"]; message: string };

/** `path` is relative to the daemon's working directory, which is the only
 * root the surface serves (docs/web-bind.md); segments are encoded here so a
 * `#` or `?` in a filename cannot re-enter URL parsing. */
export async function fetchFile(
  path: string,
  etag?: string | null,
): Promise<FileFetch> {
  const response = await apiFetch(fileUrl(path), {
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
    const envelope = (await response.json()) as ErrorBody;
    return { kind: "refused", code: envelope.code, message: envelope.message };
  } catch {
    return {
      kind: "refused",
      code: "internal",
      message: `http ${response.status}`,
    };
  }
}
