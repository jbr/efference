// The connection lifecycle (docs/control-api.md, "Connection lifecycle"):
// a bounded catch-up read, then the one long-lived stream from the cursor —
// `GET /log?tail=N`, then `GET /events?since=last_seq+1`. Reconnect inside the
// stream reader replays by cursor; a 401 anywhere parks the session as
// unauthenticated until Login mints a cookie and calls retry(). A 404 marks the
// session missing and stops retries, and so does a stream that ended on a failed
// start. The stream stays
// open while the tab is hidden: whether a person is attending is reported
// separately (attention.ts), not inferred from the connection.

import { apiBase } from "../api/base.ts";
import type { Frame } from "../api/types.ts";
import { ApiError, fetchLogTail } from "../api/http.ts";
import {
  openStream,
  type StreamCallbacks,
  type StreamHandle,
} from "../api/stream.ts";
import { applyFrame, reseed, type Store } from "./store.ts";
import { coherentWindow, HISTORY_PAGE } from "./history.ts";

/** How much of the log's end the catch-up read asks for. */
export const CATCH_UP_TAIL = HISTORY_PAGE;

export interface Connector {
  /** Re-run catch-up + stream, e.g. after the login view mints a session. */
  retry(): void;
  close(): void;
}

/**
 * What a connection does with each arriving frame. The default folds it into the
 * store at once; the dev scrubber (`record.ts`) interposes to keep a tape of the
 * frames as they arrived and control when they fold.
 *
 * `reset` marks the boundary a fresh catch-up read draws: everything the tap
 * holds from before it describes a store state that the re-seed has replaced.
 */
export interface FrameTap {
  reset(): void;
  frame(frame: Frame): void;
}

export function connect(
  store: Store,
  tap?: FrameTap,
  base: string = apiBase(),
  observer?: Partial<StreamCallbacks>,
): Connector {
  const sink: FrameTap = tap ?? {
    reset: () => {},
    frame: (frame) => store.update((s) => applyFrame(s, frame)),
  };
  let stream: StreamHandle | null = null;
  let closed = false;

  async function establish() {
    stream?.close();
    stream = null;
    store.update((s) => ({ ...s, connection: "connecting" }));
    try {
      const tail = await fetchLogTail(CATCH_UP_TAIL, base);
      if (closed) return;
      store.update((s) => reseed(s, tail));
      try {
        const entries = await coherentWindow(store.snapshot().entries, base);
        if (closed) return;
        store.update((s) => reseed(s, entries));
      } catch {
        // A missing reference is not a missing session. Retain the fetched
        // window and its cursor; backward paging can bring its owner into view.
      }
    } catch (error) {
      if (closed) return;
      if (
        error instanceof ApiError &&
        (error.status === 401 || error.status === 404)
      ) {
        store.update((s) => ({
          ...s,
          connection: error.status === 404 ? "missing" : "unauthenticated",
        }));
        return;
      }
      // The daemon is unreachable or errored; the stream opener below will
      // keep retrying, and state topics arrive on connect regardless. The
      // transcript window just starts empty until retry() re-seeds.
    }
    sink.reset();
    const since = store.snapshot().lastSeq;
    stream = openStream(
      since === null ? 0 : since + 1,
      {
        onAttempt: observer?.onAttempt,
        onActivity: observer?.onActivity,
        onFrame: (frame) => {
          observer?.onFrame?.(frame);
          sink.frame(frame);
        },
        onConnect: () => {
          observer?.onConnect?.();
          store.update((s) => ({ ...s, connection: "live" }));
        },
        onDisconnect: (error) => {
          observer?.onDisconnect?.(error);
          if (
            error instanceof ApiError &&
            (error.status === 401 || error.status === 404)
          ) {
            stream?.close();
            store.update((s) => ({
              ...s,
              connection: error.status === 404 ? "missing" : "unauthenticated",
            }));
          } else if (store.snapshot().topics.starting?.failure) {
            // A failed start ends its stream after the frame that says why, and
            // there is nothing behind it to reconnect to (docs/control-api.md).
            stream?.close();
            store.update((s) => ({ ...s, connection: "ended" }));
          } else {
            store.update((s) => ({ ...s, connection: "reconnecting" }));
          }
        },
      },
      base,
    );
  }

  void establish();
  return {
    retry: () => void establish(),
    close() {
      closed = true;
      stream?.close();
    },
  };
}
