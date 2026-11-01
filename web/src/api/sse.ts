// The event-stream reader every `fetch` stream in the client shares: a
// hand-rolled SSE parser (`sseData`) and the reconnecting loop around it
// (`readEventStream`, docs/web-client.md). The four streams — the control
// stream (`stream.ts`), the listing and an overview card's feed (`sessions.ts`),
// and the `/files` change pokes (`files.ts`) — differ in the URL each attempt
// asks for, what they do with an event, and whether they yield while
// dispatching; the abort, status, backoff and silence handling are here.

import { apiFetch, throwIfFailed } from "./http.ts";

export interface StreamHandle {
  close(): void;
}

/**
 * How long an attempt may go without a read before it is treated as dead, in
 * ms. The host sends a `:` comment on every event stream after 15 s of quiet
 * (`EVENTS_HEARTBEAT` in src/routes/http.rs, and the same interval on the
 * listing and `/files` streams), so 35 s is two missed heartbeats and a margin
 * for a slow link. Any read, an event or a comment, counts.
 */
export const STALL_DEADLINE_MS = 35_000;

/**
 * How old the last read may be, in ms, when the page becomes visible again
 * before the attempt is dropped and reopened at once rather than waiting out
 * `STALL_DEADLINE_MS`. A healthy stream has been read within one 15 s
 * heartbeat; 20 s is that interval and a margin, so only a stream that missed a
 * heartbeat while the page was away is replaced.
 */
export const VISIBLE_STALE_MS = 20_000;

export interface EventStreamOptions {
  /**
   * The URL for the next attempt, called once per attempt with that attempt's
   * signal. A stream that resumes from a cursor reads it here; one that must
   * make a request first (an overview card's cold log read) may do so before
   * answering, and the request is aborted with the attempt.
   */
  url(signal: AbortSignal): string | Promise<string>;
  /** One event's `data:` payload. A returned promise is awaited before the next
   * event is dispatched; a throw ends the attempt like any failure. */
  onEvent(data: string): void | Promise<void>;
  /** The request succeeded and its body is about to be read. Called on every
   * reconnect. */
  onConnect?(): void;
  /** The attempt ended or failed; the reader retries unless closed. */
  onDisconnect?(error: unknown): void;
  /** Diagnostic observation of each raw read, heartbeats included. */
  onActivity?(bytes: number): void;
}

/**
 * Hold `GET url` open as an event stream until `close()`, reconnecting after a
 * refused or failed request, a stream that ends, an `onEvent` that throws, a
 * silence past `STALL_DEADLINE_MS`, or the page returning to view with a stream
 * last read more than `VISIBLE_STALE_MS` ago. The wait before a retry doubles
 * from 1 s to a 15 s cap and resets once a request succeeds; a retry for the
 * page returning to view does not wait. A `401` is the page's bearer being
 * refused, handled in `apiFetch`, which clears it and closes the views that held
 * streams (docs/web-client.md).
 */
export function readEventStream(options: EventStreamOptions): StreamHandle {
  let closed = false;
  let controller = new AbortController();
  let attempt = 0;
  let lastRead = Date.now();
  // Why the reader aborted the current attempt itself, if it did.
  let aborted: "stalled" | "visible" | null = null;
  let watchdog: ReturnType<typeof setTimeout> | null = null;
  let cancelBackoff: (() => void) | null = null;

  // One timer per attempt, re-armed for the time left rather than reset on
  // every read: a read only stamps `lastRead`, and the timer decides when it
  // fires. A timer that fires late, as after a frozen page thaws, finds the
  // stamp as old as it is.
  function arm(mine: AbortController, wait: number) {
    watchdog = setTimeout(() => {
      const left = lastRead + STALL_DEADLINE_MS - Date.now();
      if (left > 0) arm(mine, left);
      else abort(mine, "stalled");
    }, wait);
  }

  function disarm() {
    if (watchdog !== null) clearTimeout(watchdog);
    watchdog = null;
  }

  function abort(mine: AbortController, why: "stalled" | "visible") {
    if (mine.signal.aborted) return;
    aborted = why;
    mine.abort();
  }

  function onVisibility() {
    if (closed || globalThis.document?.visibilityState !== "visible") return;
    // A reader waiting out a backoff has no stream to judge; try again now.
    if (cancelBackoff !== null) cancelBackoff();
    else if (Date.now() - lastRead > VISIBLE_STALE_MS) {
      abort(controller, "visible");
    }
  }

  function backoff(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(done, ms);
      function done() {
        clearTimeout(timer);
        cancelBackoff = null;
        resolve();
      }
      cancelBackoff = done;
    });
  }

  async function run() {
    globalThis.document?.addEventListener("visibilitychange", onVisibility);
    try {
      while (!closed) {
        const mine = new AbortController();
        controller = mine;
        aborted = null;
        lastRead = Date.now();
        arm(mine, STALL_DEADLINE_MS);
        try {
          const url = await options.url(mine.signal);
          const response = await apiFetch(url, {
            headers: { accept: "text/event-stream" },
            signal: mine.signal,
          });
          await throwIfFailed(response);
          if (!response.body) {
            throw new Error("events: missing response body");
          }
          if (closed) return;
          lastRead = Date.now();
          attempt = 0;
          options.onConnect?.();
          const reads = (bytes: number) => {
            lastRead = Date.now();
            options.onActivity?.(bytes);
          };
          for await (const data of sseData(response.body, reads)) {
            if (closed) return;
            const pending = options.onEvent(data);
            if (pending) await pending;
          }
          throw new Error("events: stream ended");
        } catch (error) {
          if (closed) return;
          options.onDisconnect?.(
            aborted === "stalled"
              ? new Error(
                  `events: no read for ${STALL_DEADLINE_MS / 1000} s; reconnecting`,
                )
              : aborted === "visible"
                ? new Error("events: stale on return to view; reconnecting")
                : error,
          );
          if (closed) return;
          // End the response if a callback threw and left it open.
          mine.abort();
          if (aborted === "visible") continue;
          attempt += 1;
          disarm();
          await backoff(Math.min(500 * 2 ** attempt, 15_000));
        } finally {
          disarm();
        }
      }
    } finally {
      globalThis.document?.removeEventListener(
        "visibilitychange",
        onVisibility,
      );
    }
  }

  void run();
  return {
    close() {
      closed = true;
      controller.abort();
      disarm();
      cancelBackoff?.();
    },
  };
}

/**
 * Yield the `data:` payload of each SSE event on the stream. Minimal but
 * conformant for what the daemon emits: CR/LF/CRLF line splits, multi-`data:`
 * concatenation, comment (`:`) and non-`data:` field lines ignored.
 * `onActivity` hears the size of every raw read, comments included.
 */
export async function* sseData(
  body: ReadableStream<Uint8Array>,
  onActivity?: (bytes: number) => void,
): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buffer = "";
  let dataLines: string[] = [];
  const reader = body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      onActivity?.(value.byteLength);
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split(/\r\n|\r|\n/);
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        if (line === "") {
          if (dataLines.length > 0) {
            yield dataLines.join("\n");
            dataLines = [];
          }
        } else if (line.startsWith("data:")) {
          dataLines.push(line.slice(5).replace(/^ /, ""));
        }
        // `event:`/`id:` are redundant with the envelope; `:` is the heartbeat.
      }
    }
  } finally {
    reader.releaseLock();
  }
}
