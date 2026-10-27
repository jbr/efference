import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  readEventStream,
  sseData,
  STALL_DEADLINE_MS,
  VISIBLE_STALE_MS,
} from "./sse.ts";

function streamOf(...chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
}

async function collect(body: ReadableStream<Uint8Array>): Promise<string[]> {
  const out: string[] = [];
  for await (const data of sseData(body)) out.push(data);
  return out;
}

describe("sseData", () => {
  it("yields each event's data and ignores event:/id: lines", async () => {
    const events = await collect(
      streamOf(
        'event: user\nid: 1\ndata: {"class":"log","seq":1}\n\n',
        'event: gate\ndata: {"class":"state"}\n\n',
      ),
    );
    expect(events).toEqual(['{"class":"log","seq":1}', '{"class":"state"}']);
  });

  it("reassembles events split across reads", async () => {
    const events = await collect(
      streamOf('data: {"a"', ":1}\n", "\n", 'data: {"b":2}\n\n'),
    );
    expect(events).toEqual(['{"a":1}', '{"b":2}']);
  });

  it("discards heartbeat comments without dispatching", async () => {
    const events = await collect(
      streamOf(":\n\n", "data: x\n\n", ": keepalive\n\n"),
    );
    expect(events).toEqual(["x"]);
  });

  it("reports raw reads including heartbeat-only chunks", async () => {
    const reads = vi.fn();
    const events = [];
    for await (const data of sseData(streamOf(":\n\n", "data: x\n\n"), reads))
      events.push(data);
    expect(events).toEqual(["x"]);
    expect(reads.mock.calls).toEqual([[3], [9]]);
  });

  it("joins multi-line data fields with newlines", async () => {
    const events = await collect(streamOf("data: a\ndata: b\n\n"));
    expect(events).toEqual(["a\nb"]);
  });
});

const encoder = new TextEncoder();

/** One open response the test feeds, and which errors when its request's signal
 * aborts, as the body of a real fetch does. */
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
      // Already errored; there is nothing left to abort.
    }
  });
  return {
    response: new Response(body, { status: 200 }),
    send: (text: string) => controller.enqueue(encoder.encode(text)),
  };
}

/** A fetch that opens a live response per call, recording each request's URL,
 * signal, and the response's feed. */
function liveFetch() {
  const attempts: {
    url: string;
    signal: AbortSignal;
    send: (text: string) => void;
  }[] = [];
  const fetchMock = vi.fn((url: string, init: RequestInit) => {
    const live = liveResponse(init.signal);
    attempts.push({
      url,
      signal: init.signal as AbortSignal,
      send: live.send,
    });
    return Promise.resolve(live.response);
  });
  vi.stubGlobal("fetch", fetchMock);
  return { attempts, fetchMock };
}

/** A stand-in for the page's `document`: an event target with a visibility. */
function stubDocument() {
  const doc = Object.assign(new EventTarget(), {
    visibilityState: "visible" as "visible" | "hidden",
  });
  vi.stubGlobal("document", doc);
  return {
    show() {
      doc.visibilityState = "visible";
      doc.dispatchEvent(new Event("visibilitychange"));
    },
    hide() {
      doc.visibilityState = "hidden";
      doc.dispatchEvent(new Event("visibilitychange"));
    },
  };
}

const advance = (ms: number) => vi.advanceTimersByTimeAsync(ms);

/** A reader over `/x?since=<cursor>`, whose cursor is the last event heard
 * plus one, like the control stream's. */
function cursorReader() {
  let cursor = 0;
  const disconnects: unknown[] = [];
  const handle = readEventStream({
    url: () => `/x?since=${cursor}`,
    onEvent: (data) => {
      cursor = Number(data) + 1;
    },
    onDisconnect: (error) => disconnects.push(error),
  });
  return { handle, disconnects };
}

describe("the reconnecting reader's watchdog", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("keeps one attempt open while heartbeats arrive", async () => {
    const { attempts, fetchMock } = liveFetch();
    const { handle, disconnects } = cursorReader();
    await advance(0);
    // Four deadlines' worth of time, with a heartbeat every 15 s.
    for (let elapsed = 0; elapsed < 4 * STALL_DEADLINE_MS; elapsed += 15_000) {
      attempts[0]!.send(":\n\n");
      await advance(15_000);
    }
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(attempts[0]!.signal.aborted).toBe(false);
    expect(disconnects).toEqual([]);
    handle.close();
  });

  it("aborts a silent attempt at the deadline and reopens it from the cursor", async () => {
    const { attempts, fetchMock } = liveFetch();
    const { handle, disconnects } = cursorReader();
    await advance(0);
    attempts[0]!.send("data: 6\n\n");
    await advance(30_000);
    // A comment is a read: it moves the deadline to 35 s after itself.
    attempts[0]!.send(":\n\n");
    await advance(STALL_DEADLINE_MS - 1);
    expect(attempts[0]!.signal.aborted).toBe(false);

    await advance(1);
    expect(attempts[0]!.signal.aborted).toBe(true);
    expect(disconnects).toHaveLength(1);
    expect(String(disconnects[0])).toContain("no read");

    // The ordinary path: the first retry waits one second.
    await advance(999);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(attempts.map((a) => a.url)).toEqual(["/x?since=0", "/x?since=7"]);
    expect(attempts[1]!.signal.aborted).toBe(false);
    handle.close();
  });

  it("reconnects at once when the page returns after a stale read", async () => {
    const page = stubDocument();
    const { attempts, fetchMock } = liveFetch();
    const { handle } = cursorReader();
    await advance(0);
    attempts[0]!.send("data: 3\n\n");
    page.hide();
    await advance(VISIBLE_STALE_MS + 1);
    expect(attempts[0]!.signal.aborted).toBe(false);

    page.show();
    await advance(0);
    expect(attempts[0]!.signal.aborted).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(attempts[1]!.url).toBe("/x?since=4");
    handle.close();
  });

  it("leaves a recently read stream alone when the page returns", async () => {
    const page = stubDocument();
    const { attempts, fetchMock } = liveFetch();
    const { handle } = cursorReader();
    await advance(0);
    attempts[0]!.send("data: 3\n\n");
    page.hide();
    await advance(5_000);
    page.show();
    await advance(0);
    expect(attempts[0]!.signal.aborted).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // Becoming hidden is never a reason to reconnect.
    await advance(VISIBLE_STALE_MS);
    page.hide();
    await advance(0);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    handle.close();
  });

  it("tries again at once when the page returns during a backoff", async () => {
    const page = stubDocument();
    const fetchMock = vi.fn(() => Promise.reject(new TypeError("offline")));
    vi.stubGlobal("fetch", fetchMock);
    const { handle } = cursorReader();
    await advance(0);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    page.show();
    await advance(0);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    handle.close();
  });

  it("stops listening for visibility and timing when closed", async () => {
    const page = stubDocument();
    const { attempts, fetchMock } = liveFetch();
    const { handle, disconnects } = cursorReader();
    await advance(0);
    handle.close();
    await advance(STALL_DEADLINE_MS * 2);
    page.show();
    await advance(60_000);
    expect(attempts[0]!.signal.aborted).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(disconnects).toEqual([]);
  });
});
