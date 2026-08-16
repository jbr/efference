// The connection lifecycle (docs/control-api.md, "Connection lifecycle"):
// a bounded catch-up read, then the one long-lived stream from the cursor —
// `GET /log?tail=N`, then `GET /events?since=last_seq+1`. Reconnect inside the
// stream reader replays by cursor; a 401 anywhere parks the session as
// unauthenticated until Login mints a cookie and calls retry().

import { ApiError, fetchLogTail } from "../api/http.ts";
import { openStream, type StreamHandle } from "../api/stream.ts";
import { applyFrame, seed, type Store } from "./store.ts";

const TAIL = 200;

export interface Connector {
  /** Re-run catch-up + stream, e.g. after the login view mints a session. */
  retry(): void;
  close(): void;
}

export function connect(store: Store): Connector {
  let stream: StreamHandle | null = null;
  let closed = false;

  async function open() {
    stream?.close();
    stream = null;
    store.update((s) => ({ ...s, connection: "connecting" }));
    try {
      const entries = await fetchLogTail(TAIL);
      if (closed) return;
      store.update((s) => seed(s, entries));
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
    const since = store.snapshot().lastSeq;
    stream = openStream(since === null ? 0 : since + 1, {
      onFrame: (frame) => store.update((s) => applyFrame(s, frame)),
      onConnect: () => store.update((s) => ({ ...s, connection: "live" })),
      onDisconnect: () =>
        store.update((s) => ({ ...s, connection: "reconnecting" })),
    });
  }

  void open();
  return {
    retry: () => void open(),
    close() {
      closed = true;
      stream?.close();
    },
  };
}
