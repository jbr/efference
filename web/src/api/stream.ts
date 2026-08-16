// The control-stream reader: `GET /v1/events` over fetch, with a hand-rolled
// SSE parser (docs/control-api.md).
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

import type { Frame, Seq } from "./types.ts";

export interface StreamHandle {
  close(): void;
}

export interface StreamCallbacks {
  onFrame(frame: Frame): void;
  /** Connection ended or failed; the reader will retry unless closed. */
  onDisconnect?(error: unknown): void;
  onConnect?(): void;
}

/**
 * Open the stream from `since`, dispatching every frame, reconnecting with a
 * capped backoff. The resume cursor advances to `seq + 1` past each log frame
 * (the durable subset; docs/control-api.md), so a reconnect replays exactly.
 */
export function openStream(
  since: Seq,
  callbacks: StreamCallbacks,
): StreamHandle {
  let cursor = since;
  let closed = false;
  let controller = new AbortController();
  let attempt = 0;

  async function run() {
    while (!closed) {
      controller = new AbortController();
      try {
        const response = await fetch(`/v1/events?since=${cursor}`, {
          headers: { accept: "text/event-stream" },
          signal: controller.signal,
        });
        if (!response.ok || !response.body) {
          throw new Error(`events: http ${response.status}`);
        }
        attempt = 0;
        callbacks.onConnect?.();
        for await (const data of sseData(response.body)) {
          const frame = JSON.parse(data) as Frame;
          if (frame.class === "log") {
            cursor = frame.seq + 1;
          }
          callbacks.onFrame(frame);
        }
        throw new Error("events: stream ended");
      } catch (error) {
        if (closed) return;
        callbacks.onDisconnect?.(error);
        attempt += 1;
        await sleep(Math.min(500 * 2 ** attempt, 15_000));
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

/**
 * Yield the `data:` payload of each SSE event on the stream. Minimal but
 * conformant for what the daemon emits: CR/LF/CRLF line splits, multi-`data:`
 * concatenation, comment (`:`) and non-`data:` field lines ignored.
 */
export async function* sseData(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buffer = "";
  let dataLines: string[] = [];
  const reader = body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
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

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
