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
import type { Frame, LogEntry, Seq, StateSnapshot } from "./types.ts";

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

/** The state topics an overview card reads. */
const CARD_TOPICS = [
  "gate",
  "ask",
  "stall",
  "running",
  "usage",
  "session_identity",
  "upstream_retry",
];

/** How many entries a card reads cold: enough to reach back past a run of
 * tool results to the last assistant entry, which carries both the latest
 * intent and the latest prose. */
const CARD_TAIL = 12;

export interface CardFeed {
  state: StateSnapshot;
  /** The log entries read so far, oldest first; the caller folds them. */
  entries: LogEntry[];
}

/**
 * Follow one hosted session for its overview card: a cold `GET /log?tail=`
 * read, then `/v1/events` from just past it carrying the card's state topics
 * and the log (docs/control-api.md, "opening cold"). `onFeed` hears the
 * accumulated state and each batch of new log entries.
 *
 * `presence=off`: an overview glancing at a session is not someone watching
 * it, so it must not suppress that session's push notifications
 * (docs/web-push.md). One of these per card until the host serves an overview
 * stream (backlog/host-overview-stream.md).
 */
export function openSessionCard(
  id: string,
  onFeed: (feed: CardFeed) => void,
): SessionsHandle {
  let closed = false;
  let controller = new AbortController();
  let attempt = 0;
  let cursor: Seq | null = null;
  const base = `/sessions/${encodeURIComponent(id)}/v1`;
  const include = ["log", ...CARD_TOPICS.map((topic) => `state:${topic}`)].join(
    ",",
  );

  async function run() {
    while (!closed) {
      controller = new AbortController();
      const state: StateSnapshot = { last_seq: null };
      try {
        if (cursor === null) {
          const response = await fetch(`${base}/log?tail=${CARD_TAIL}`, {
            signal: controller.signal,
          });
          if (!response.ok) throw new Error(`log: http ${response.status}`);
          const entries = (await response.json()) as LogEntry[];
          cursor = (entries.at(-1)?.seq ?? -1) + 1;
          onFeed({ state: { ...state }, entries });
        }
        const response = await fetch(
          `${base}/events?include=${include}&since=${cursor}&presence=off`,
          {
            headers: { accept: "text/event-stream" },
            signal: controller.signal,
          },
        );
        if (!response.ok || !response.body) {
          throw new Error(`events: http ${response.status}`);
        }
        attempt = 0;
        for await (const data of sseData(response.body)) {
          const frame = JSON.parse(data) as Frame;
          if (frame.class === "log") {
            cursor = frame.seq + 1;
            onFeed({ state: { ...state }, entries: [frame.data] });
          } else if (frame.class === "state") {
            (state as unknown as Record<string, unknown>)[frame.kind] =
              frame.data;
            onFeed({ state: { ...state }, entries: [] });
          }
        }
        throw new Error("events: stream ended");
      } catch {
        if (closed) return;
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
