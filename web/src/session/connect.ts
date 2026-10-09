// The connection lifecycle (docs/control-api.md, "Connection lifecycle"):
// a bounded catch-up read, then the one long-lived stream from the cursor —
// `GET /log?tail=N`, then `GET /events?since=last_seq+1`. Reconnect inside the
// stream reader replays by cursor; a 401 anywhere parks the session as
// unauthenticated until Login mints a cookie and calls retry(). The stream stays
// open while the tab is hidden: whether a person is attending is reported
// separately (attention.ts), not inferred from the connection.

import { apiBase } from "../api/base.ts";
import type { Frame } from "../api/types.ts";
import { ApiError, fetchLogTail } from "../api/http.ts";
import { openStream, type StreamHandle } from "../api/stream.ts";
import { applyFrame, reseed, type Store } from "./store.ts";

/** How much of the log's end the catch-up read asks for. */
export const CATCH_UP_TAIL = 200;

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
      const entries = await fetchLogTail(CATCH_UP_TAIL, base);
      if (closed) return;
      store.update((s) => reseed(s, entries));
    } catch (error) {
      if (closed) return;
      if (error instanceof ApiError && error.status === 401) {
        store.update((s) => ({ ...s, connection: "unauthenticated" }));
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
        onFrame: (frame) => sink.frame(frame),
        onConnect: () => store.update((s) => ({ ...s, connection: "live" })),
        onDisconnect: () =>
          store.update((s) => ({ ...s, connection: "reconnecting" })),
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
