import { describe, expect, it, vi } from "vitest";
import { openStream } from "./stream.ts";

function streamOf(...chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
}

describe("the dispatch loop", () => {
  /** A macrotask posted before the loop starts. Task queues are FIFO, so if
   * the loop ever returns to the task queue this runs before the yield that
   * put it there resolves; if it never does, this runs only once the whole
   * burst has been dispatched. */
  function taskMarker(): Promise<void> {
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

  const burn = (ms: number) => {
    const until = performance.now() + ms;
    while (performance.now() < until);
  };

  it("gives the task queue a turn during a burst it cannot outrun", async () => {
    // Every step of the loop is a promise continuation — `for await` resumes on
    // a microtask, and sseData yields each event parsed out of one read without
    // awaiting anything else — so without an explicit yield a producer faster
    // than the consumer holds the thread against timers, input, paint, and the
    // animation frame the store's coalescing flushes on
    // (docs/web-client.md).
    const total = 8;
    const body = streamOf(
      ...Array.from(
        { length: total },
        (_, i) => `data: {"class":"x","i":${i}}\n\n`,
      ),
    );
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(body, { status: 200 })),
    );
    let dispatched = 0;
    let atMarker: number | null = null;
    const done = new Promise<void>((resolve) => {
      const handle = openStream(0, {
        onFrame: () => {
          dispatched += 1;
          burn(4);
        },
        onDisconnect: () => {
          handle.close();
          resolve();
        },
      });
      void taskMarker().then(() => {
        atMarker ??= dispatched;
      });
    });
    await done;
    expect(dispatched).toBe(total);
    expect(atMarker).not.toBeNull();
    expect(atMarker).toBeLessThan(total);
    vi.unstubAllGlobals();
  });
});
