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

import { readEventStream } from "./sse.ts";
import { apiFetch } from "./http.ts";
import type { Frame, StateSnapshot } from "./frames.ts";
import type { LogEntry, Seq } from "./entry.ts";
import type { ListedSession } from "./types.ts";

export interface SessionsHandle {
  close(): void;
}

interface SessionsReader {
  onSessions: (sessions: ListedSession[]) => void;
  onError?: (error: unknown) => void;
}

// One stream serves every reader: the switcher's tabs, the overview list, and a
// session view's own row all read the same whole listing, and a reader per call
// would carry the same frames twice. The host sends the listing on connect, so
// a reader that joins mid-stream is served at once.
const readers = new Set<SessionsReader>();
let following: (() => void) | null = null;

/**
 * Subscribe to the listing, calling `onSessions` with each whole value and
 * reconnecting with a capped backoff. `onError` hears a dropped connection; the
 * next successful frame is the all-clear, so a caller can show the last listing
 * it had rather than blanking the page.
 */
export function openSessions(
  onSessions: (sessions: ListedSession[]) => void,
  onError?: (error: unknown) => void,
): SessionsHandle {
  const reader: SessionsReader = { onSessions, onError };
  readers.add(reader);
  following ??= followSessions();
  return {
    close() {
      readers.delete(reader);
      if (readers.size > 0) return;
      // The stream stops with the last reader; the next one starts it again.
      following?.();
      following = null;
    },
  };
}

/** The stream itself, from the first reader to the last. Answers its stop. */
function followSessions(): () => void {
  const stream = readEventStream({
    url: () => "/sessions",
    onEvent(data) {
      const listing = JSON.parse(data) as ListedSession[];
      for (const reader of readers) reader.onSessions(listing);
    },
    onDisconnect(error) {
      for (const reader of readers) reader.onError?.(error);
    },
  });
  return () => stream.close();
}

/** The state topics an overview card reads. */
const CARD_TOPICS = [
  "gate",
  "ask",
  "stall",
  "loop_phase",
  "cold",
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
 * One of these per card until the host serves an overview stream
 * (backlog/inbox/host-overview-stream.md).
 */
export function openSessionCard(
  id: string,
  onFeed: (feed: CardFeed) => void,
): SessionsHandle {
  let cursor: Seq | null = null;
  let state: StateSnapshot = { last_seq: null };
  const base = `/sessions/${encodeURIComponent(id)}/v1`;
  const include = ["log", ...CARD_TOPICS.map((topic) => `state:${topic}`)].join(
    ",",
  );

  return readEventStream({
    async url(signal) {
      // The host replays every state topic on connect, so a reconnect starts
      // the accumulated state over.
      state = { last_seq: null };
      if (cursor === null) {
        const response = await apiFetch(`${base}/log?tail=${CARD_TAIL}`, {
          signal,
        });
        if (!response.ok) throw new Error(`log: http ${response.status}`);
        const entries = (await response.json()) as LogEntry[];
        cursor = (entries.at(-1)?.seq ?? -1) + 1;
        onFeed({ state: { ...state }, entries });
      }
      return `${base}/events?include=${include}&since=${cursor}`;
    },
    onEvent(data) {
      const frame = JSON.parse(data) as Frame;
      if (frame.class === "log") {
        cursor = frame.seq + 1;
        onFeed({ state: { ...state }, entries: [frame.data] });
      } else if (frame.class === "state") {
        (state as unknown as Record<string, unknown>)[frame.kind] = frame.data;
        onFeed({ state: { ...state }, entries: [] });
      }
    },
  });
}
