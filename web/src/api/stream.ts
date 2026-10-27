// The control-stream reader: `GET /v1/events` over fetch, with the hand-rolled
// SSE parser and reconnecting loop of `sse.ts` (docs/control-api.md).
//
// Not `EventSource`, for two reasons. The daemon sets `event:` to the frame's
// kind, and `EventSource` dispatches named events — a client would register one
// listener per known kind and silently drop frames from a newer daemon, the
// exact failure the API's own unknown-token rule exists to prevent. And its
// automatic reconnect resumes via the `Last-Event-ID` header, while the
// daemon's resume handshake is `?since=` — so reconnection is managed here
// either way. The envelope carries `class`/`kind`/`seq`, making the `event:`
// and `id:` SSE fields redundant for this parser: only `data:` lines matter,
// and `:` heartbeat comments fall out of the grammar for free.

import { apiBase } from "./base.ts";
import { readEventStream, type StreamHandle } from "./sse.ts";
import type { Frame, Seq } from "./types.ts";

export interface StreamCallbacks {
  onFrame(frame: Frame): void;
  /** Diagnostic observation of a request and raw reads, including heartbeats. */
  onAttempt?(cursor: Seq): void;
  onActivity?(bytes: number): void;
  /** Connection ended or failed; the reader will retry unless closed. */
  onDisconnect?(error: unknown): void;
  onConnect?(): void;
}

/**
 * Open the stream from `since`, dispatching every frame, reconnecting with a
 * capped backoff (`readEventStream`, which also reopens a stream that has gone
 * silent). The resume cursor advances to `seq + 1` past each log frame
 * (the durable subset; docs/control-api.md), so a reconnect replays exactly.
 * `base` is the session the stream belongs to, fixed when it opens: a reconnect
 * after the page has moved to another session must still reach this one.
 */
export function openStream(
  since: Seq,
  callbacks: StreamCallbacks,
  base: string = apiBase(),
): StreamHandle {
  let cursor = since;
  let sliceStart = performance.now();

  return readEventStream({
    url() {
      callbacks.onAttempt?.(cursor);
      return `${base}/v1/events?since=${cursor}`;
    },
    onConnect() {
      callbacks.onConnect?.();
      sliceStart = performance.now();
    },
    onActivity: callbacks.onActivity,
    onDisconnect: callbacks.onDisconnect,
    onEvent(data) {
      const frame = JSON.parse(data) as Frame;
      if (frame.class === "log") {
        cursor = frame.seq + 1;
      }
      callbacks.onFrame(frame);
      if (performance.now() - sliceStart < SLICE_MS) return;
      return yieldToTasks().then(() => {
        sliceStart = performance.now();
      });
    },
  });
}

/**
 * How long the dispatch loop may run before it gives the browser a task slice,
 * in ms (docs/web-client.md, "Coalescing the fold").
 *
 * The dispatch loop (`readEventStream`, with `onEvent` below) is built out of
 * promise continuations end to end: `for await` resumes on a microtask, and `sseData` yields every event it parsed out of one
 * read without awaiting anything else. Microtasks run to exhaustion before the
 * task queue is looked at, so while a producer delivers frames faster than they
 * are consumed the loop holds the thread against input, timers, animation
 * frames, and paint alike — including the animation frame the store's
 * coalescing schedules its flush on, which is what would make the batching
 * above unobservable. An `await` on a task-queue round trip is what re-opens
 * that window; the interval is wall clock rather than a frame count so the
 * guarantee ("a slice at least this often") does not depend on what a frame
 * costs.
 */
const SLICE_MS = 8;

/**
 * Resume on the task queue rather than the microtask queue. `MessageChannel`
 * rather than `setTimeout`, whose nested-timer clamp would floor each yield at
 * ~4 ms — a third of the slice above spent asleep.
 */
function yieldToTasks(): Promise<void> {
  if (typeof MessageChannel !== "function") {
    return new Promise((resolve) => setTimeout(resolve, 0));
  }
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      channel.port1.close();
      channel.port2.close();
      resolve();
    };
    channel.port2.postMessage(null);
  });
}
