import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dirStream, fileStream, type ChangeStreamCallbacks } from "./files.ts";

const encoder = new TextEncoder();

/** One response whose body the test feeds and ends, and which errors when the
 * request's signal aborts — as the body of a real fetch does. */
function liveResponse(signal: AbortSignal | null | undefined) {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
    },
  });
  signal?.addEventListener("abort", () => {
    try {
      controller.error(new DOMException("aborted", "AbortError"));
    } catch {
      // Already closed or errored; there is nothing left to abort.
    }
  });
  return {
    response: new Response(body, { status: 200 }),
    send: (text: string) => controller.enqueue(encoder.encode(text)),
    end: () => controller.close(),
  };
}

type Attempt = (init: RequestInit) => Promise<Response>;

/** Stub `fetch` with one planned behaviour per attempt, in order. */
function plan(...attempts: Attempt[]) {
  const queue = [...attempts];
  const fetchMock = vi.fn((_url: string, init: RequestInit) => {
    const next = queue.shift();
    if (next === undefined) return new Promise<Response>(() => {});
    return next(init);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function recorder() {
  const events: string[] = [];
  const callbacks: ChangeStreamCallbacks = {
    onOpen: () => events.push("open"),
    onChange: (path) => events.push(`poke ${path}`),
  };
  return { events, callbacks };
}

/** Let promise continuations (and the stream reads behind them) run. */
const settle = () => vi.advanceTimersByTimeAsync(0);

const refusal = () =>
  Promise.resolve(
    new Response(
      JSON.stringify({ code: "not_found", message: "no such path" }),
      { status: 404 },
    ),
  );

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("the change stream", () => {
  it("asks for an event stream at the path's URL", async () => {
    const fetchMock = plan((init) =>
      Promise.resolve(liveResponse(init.signal).response),
    );
    const { callbacks } = recorder();
    const stream = fileStream("src/a b/#c.rs", callbacks);
    await settle();
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("/files/src/a%20b/%23c.rs");
    expect(init.headers).toEqual({ accept: "text/event-stream" });
    expect(init.signal).toBeInstanceOf(AbortSignal);
    stream.close();
  });

  it("addresses the root directory as /files/", async () => {
    const fetchMock = plan((init) =>
      Promise.resolve(liveResponse(init.signal).response),
    );
    const stream = dirStream("", recorder().callbacks);
    await settle();
    expect(fetchMock.mock.calls[0][0]).toBe("/files/");
    stream.close();
  });

  it("reports the connect as an open, then each event as a poke with its path", async () => {
    let feed!: ReturnType<typeof liveResponse>;
    plan((init) => {
      feed = liveResponse(init.signal);
      return Promise.resolve(feed.response);
    });
    const { events, callbacks } = recorder();
    const stream = fileStream("a.rs", callbacks);
    await settle();
    expect(events).toEqual(["open"]);

    // The server's framing: `event: change`, the path as data, and idle
    // comments between pokes.
    feed.send("event: change\ndata: a.rs\n\n");
    await settle();
    feed.send(":\n\n");
    feed.send("event: change\ndata: dir/b.rs\n\n");
    await settle();
    expect(events).toEqual(["open", "poke a.rs", "poke dir/b.rs"]);
    stream.close();
  });

  it("reports a poke for the root directory, whose path is empty", async () => {
    let feed!: ReturnType<typeof liveResponse>;
    plan((init) => {
      feed = liveResponse(init.signal);
      return Promise.resolve(feed.response);
    });
    const { events, callbacks } = recorder();
    const stream = dirStream("", callbacks);
    await settle();
    feed.send("event: change\ndata:\n\n");
    await settle();
    expect(events).toEqual(["open", "poke "]);
    stream.close();
  });

  it("opens again on every reconnect after the stream ends", async () => {
    let first!: ReturnType<typeof liveResponse>;
    plan(
      (init) => {
        first = liveResponse(init.signal);
        return Promise.resolve(first.response);
      },
      (init) => Promise.resolve(liveResponse(init.signal).response),
    );
    const { events, callbacks } = recorder();
    const stream = fileStream("a.rs", callbacks);
    await settle();
    first.send("event: change\ndata: a.rs\n\n");
    first.end();
    await settle();
    expect(events).toEqual(["open", "poke a.rs"]);

    // The first retry waits one second.
    await vi.advanceTimersByTimeAsync(999);
    expect(events).toEqual(["open", "poke a.rs"]);
    await vi.advanceTimersByTimeAsync(1);
    expect(events).toEqual(["open", "poke a.rs", "open"]);
    stream.close();
  });

  it("reconnects after a refusal, and opens only once a request succeeds", async () => {
    const fetchMock = plan(refusal, refusal, (init) =>
      Promise.resolve(liveResponse(init.signal).response),
    );
    const { events, callbacks } = recorder();
    const stream = fileStream("gone/a.rs", callbacks);
    await settle();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(events).toEqual([]);

    // Waits double: 1 s, then 2 s.
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(events).toEqual([]);
    await vi.advanceTimersByTimeAsync(1999);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(events).toEqual(["open"]);
    stream.close();
  });

  it("reconnects after a request that fails outright, and backs off to a cap", async () => {
    const offline: Attempt = () => Promise.reject(new TypeError("offline"));
    const fetchMock = plan(...Array.from({ length: 6 }, () => offline));
    const stream = fileStream("a.rs", recorder().callbacks);
    await settle();
    // 1, 2, 4, 8, 15 (capped from 16), 15 seconds.
    for (const [wait, calls] of [
      [1000, 2],
      [2000, 3],
      [4000, 4],
      [8000, 5],
      [15_000, 6],
    ]) {
      await vi.advanceTimersByTimeAsync(wait - 1);
      expect(fetchMock).toHaveBeenCalledTimes(calls - 1);
      await vi.advanceTimersByTimeAsync(1);
      expect(fetchMock).toHaveBeenCalledTimes(calls);
    }
    stream.close();
  });

  it("starts the backoff over once a connection has been made", async () => {
    let first!: ReturnType<typeof liveResponse>;
    const fetchMock = plan(
      refusal,
      (init) => {
        first = liveResponse(init.signal);
        return Promise.resolve(first.response);
      },
      refusal,
    );
    const stream = fileStream("a.rs", recorder().callbacks);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    first.end();
    await settle();
    // After one failure and a connection, the wait is the first step again
    // (1 s), not the second (2 s).
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    stream.close();
  });

  it("close() aborts the request and does not reconnect", async () => {
    let signal: AbortSignal | null | undefined;
    const fetchMock = plan((init) => {
      signal = init.signal;
      return Promise.resolve(liveResponse(init.signal).response);
    });
    const { events, callbacks } = recorder();
    const stream = fileStream("a.rs", callbacks);
    await settle();
    expect(signal?.aborted).toBe(false);
    stream.close();
    expect(signal?.aborted).toBe(true);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(events).toEqual(["open"]);
  });

  it("close() while waiting to reconnect cancels the attempt", async () => {
    const fetchMock = plan(refusal, (init) =>
      Promise.resolve(liveResponse(init.signal).response),
    );
    const { events, callbacks } = recorder();
    const stream = fileStream("a.rs", callbacks);
    await settle();
    stream.close();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(events).toEqual([]);
  });

  it("close() before the response arrives opens nothing", async () => {
    let answer!: (response: Response) => void;
    const { events, callbacks } = recorder();
    plan(
      () =>
        new Promise<Response>((resolve) => {
          answer = resolve;
        }),
    );
    const stream = fileStream("a.rs", callbacks);
    await settle();
    stream.close();
    // A response that lands after the close — one the abort did not stop.
    answer(liveResponse(null).response);
    await settle();
    expect(events).toEqual([]);
  });

  it("ends the connection and reconnects when a callback throws", async () => {
    const signals: (AbortSignal | null | undefined)[] = [];
    const fetchMock = plan(
      (init) => {
        signals.push(init.signal);
        return Promise.resolve(liveResponse(init.signal).response);
      },
      (init) => {
        signals.push(init.signal);
        return Promise.resolve(liveResponse(init.signal).response);
      },
    );
    const opens: string[] = [];
    const stream = fileStream("a.rs", {
      onOpen: () => {
        opens.push("open");
        if (opens.length === 1) throw new Error("a consumer bug");
      },
      onChange: () => {},
    });
    await settle();
    expect(opens).toEqual(["open"]);
    expect(signals[0]?.aborted).toBe(true);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(opens).toEqual(["open", "open"]);
    expect(signals[1]?.aborted).toBe(false);
    stream.close();
  });

  it("reports no poke that was already read when the stream was closed", async () => {
    let feed!: ReturnType<typeof liveResponse>;
    plan((init) => {
      feed = liveResponse(init.signal);
      return Promise.resolve(feed.response);
    });
    const events: string[] = [];
    const stream = fileStream("a.rs", {
      onOpen: () => {},
      onChange: (path) => {
        events.push(path);
        stream.close();
      },
    });
    await settle();
    // Two pokes in one read; the first one's handler closes the stream.
    feed.send("data: one\n\ndata: two\n\n");
    await settle();
    expect(events).toEqual(["one"]);
  });
});
