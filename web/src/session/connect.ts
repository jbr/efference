// The connection lifecycle (docs/control-api.md, "Connection lifecycle"):
// a bounded catch-up read, then the one long-lived stream from the cursor —
// `GET /log?tail=N`, then `GET /events?since=last_seq+1`. Reconnect inside the
// stream reader replays by cursor; a 401 anywhere parks the session as
// unauthenticated until Login mints a cookie and calls retry().

import { apiBase } from "../api/base.ts";
import type { Frame } from "../api/types.ts";
import { ApiError, fetchLogTail } from "../api/http.ts";
import { openStream, type StreamHandle } from "../api/stream.ts";
import { applyFrame, reseed, type Store } from "./store.ts";

/** How much of the log's end the catch-up read asks for. */
export const CATCH_UP_TAIL = 200;

/**
 * How long a hidden tab keeps its stream before dropping it, in ms.
 *
 * A live stream connection is what tells the daemon someone is watching
 * (docs/web-push.md), so a tab nobody is looking at must not hold one — otherwise
 * a pocketed phone suppresses the push it exists to receive. The linger is the
 * debounce: alt-tabbing to check something and coming straight back is one glance,
 * not a disconnect, and each reconnect costs a tail read plus a fresh state
 * snapshot. A few seconds is long enough to cover that flip and short enough that
 * a phone put away starts notifying on the next event. The service worker's
 * receiver-side skip covers what is left — an event landing inside this window,
 * where the daemon still counts a watcher who has stopped watching.
 */
const HIDDEN_LINGER_MS = 3_000;

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
  let opening = false;
  let linger: ReturnType<typeof setTimeout> | null = null;

  /** Catch up and open the stream, guarding `opening` so a second trigger (a
   * retry, a tab becoming visible) cannot start a parallel connection during the
   * awaits, when `stream` is still null. */
  async function open() {
    opening = true;
    try {
      await establish();
    } finally {
      opening = false;
      // The tab may have gone hidden during the awaits — a linger that fired then
      // found no stream to close, and the one just opened would outlive the
      // hidden tab. Re-arm so it drops on the usual schedule.
      if (!closed && document.visibilityState === "hidden" && linger === null) {
        linger = setTimeout(dropForHidden, HIDDEN_LINGER_MS);
      }
    }
  }

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

  /** Drop the stream a hidden tab has stopped earning. Nothing else is torn down:
   * the store keeps its entries and its cursor, which is what the resume below
   * reconnects from. */
  function dropForHidden() {
    linger = null;
    stream?.close();
    stream = null;
    store.update((s) => ({ ...s, connection: "hidden" }));
  }

  /**
   * Hidden and visible, as the daemon's watched count sees them
   * (docs/web-push.md): hidden arms the linger, visible either cancels a pending
   * one (the tab flip — nothing was dropped, so there is nothing to resume) or
   * reconnects through the ordinary since-cursor path. Hidden and offline
   * therefore repair identically; there is no second reconnect.
   */
  function onVisibility() {
    if (closed) return;
    if (document.visibilityState === "hidden") {
      if (linger === null) linger = setTimeout(dropForHidden, HIDDEN_LINGER_MS);
      return;
    }
    if (linger !== null) {
      clearTimeout(linger);
      linger = null;
      return;
    }
    if (stream === null && !opening) void open();
  }

  void open();
  document.addEventListener("visibilitychange", onVisibility);
  // A tab restored in the background never fires the event, so the initial state
  // is read the same way a change is: hidden at load arms the linger at once.
  onVisibility();
  return {
    retry: () => void open(),
    close() {
      closed = true;
      if (linger !== null) clearTimeout(linger);
      document.removeEventListener("visibilitychange", onVisibility);
      stream?.close();
    },
  };
}
