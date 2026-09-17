// The host's session listing as a subscription (docs/web-client.md).
//
// `GET /sessions` is one resource with two readings: asked for JSON it answers
// the listing, and asked with `Accept: text/event-stream` it answers the
// listing now and again on every change. This is the second reading, so a list
// left open reflects a session created or ended anywhere — by another client,
// by a session commissioning one, by a host reaping one — without a poll and
// without a reload button.
//
// Level-triggered, so there is no cursor and nothing to reconcile: each frame
// carries the whole listing, a missed frame is repaired by the next, and a
// reconnect is just a fresh subscription. That is what makes the retry below a
// plain backoff rather than a resume handshake.

import { sseData } from "./stream.ts";
import type { SessionEntry } from "./http.ts";

export interface SessionsHandle {
  close(): void;
}

/**
 * Subscribe to the listing, calling `onSessions` with each whole value and
 * reconnecting with a capped backoff. `onError` hears a dropped connection; the
 * next successful frame is the all-clear, so a caller can show the last listing
 * it had rather than blanking the page.
 */
export function openSessions(
  onSessions: (sessions: SessionEntry[]) => void,
  onError?: (error: unknown) => void,
): SessionsHandle {
  let closed = false;
  let controller = new AbortController();
  let attempt = 0;

  async function run() {
    while (!closed) {
      controller = new AbortController();
      try {
        const response = await fetch("/sessions", {
          headers: { accept: "text/event-stream" },
          signal: controller.signal,
        });
        if (!response.ok || !response.body) {
          throw new Error(`sessions: http ${response.status}`);
        }
        attempt = 0;
        for await (const data of sseData(response.body)) {
          onSessions(JSON.parse(data) as SessionEntry[]);
        }
        throw new Error("sessions: stream ended");
      } catch (error) {
        if (closed) return;
        onError?.(error);
        attempt += 1;
        await new Promise((resolve) =>
          setTimeout(resolve, Math.min(500 * 2 ** attempt, 15_000)),
        );
      }
    }
  }

  void run();
  return {
    close() {
      closed = true;
      controller.abort();
    },
  };
}
