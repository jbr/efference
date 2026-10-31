import { afterEach, describe, expect, it, vi } from "vitest";
import type { ListedSession } from "./types.ts";
import { openSessions } from "./sessions.ts";

/** A listing stream the test drives frame by frame, and that stays open so the
 * subscription never falls into its reconnect backoff. */
function liveStream() {
  const encoder = new TextEncoder();
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start: (started) => {
      controller = started;
    },
  });
  return {
    body,
    send(listing: ListedSession[]) {
      controller.enqueue(
        encoder.encode(`data: ${JSON.stringify(listing)}\n\n`),
      );
    },
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function listing(...ids: string[]): ListedSession[] {
  return ids.map((id) => ({
    id,
    cwd: "/project",
    status: "idle",
    processes: 0,
  }));
}

describe("openSessions", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("carries one stream for every reader", async () => {
    const stream = liveStream();
    const fetchMock = vi.fn(async () => ({ ok: true, body: stream.body }));
    vi.stubGlobal("fetch", fetchMock);
    const tabs: ListedSession[][] = [];
    const own: ListedSession[][] = [];
    const first = openSessions((value) => tabs.push(value));
    const second = openSessions((value) => own.push(value));
    await settle();

    stream.send(listing("a"));
    await settle();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(tabs).toEqual([listing("a")]);
    expect(own).toEqual([listing("a")]);

    // A reader leaving is not the others' stream ending.
    first.close();
    stream.send(listing("a", "b"));
    await settle();
    expect(tabs).toHaveLength(1);
    expect(own).toEqual([listing("a"), listing("a", "b")]);

    // The last reader out stops it.
    const call = fetchMock.mock.calls[0] as unknown as [
      string,
      { signal: AbortSignal },
    ];
    second.close();
    expect(call[1].signal.aborted).toBe(true);
  });

  it("starts a fresh stream for the next reader", async () => {
    const stream = liveStream();
    const fetchMock = vi.fn(async () => ({ ok: true, body: stream.body }));
    vi.stubGlobal("fetch", fetchMock);
    const heard: ListedSession[][] = [];
    const handle = openSessions((value) => heard.push(value));
    await settle();
    stream.send(listing("a"));
    await settle();
    handle.close();

    const next = liveStream();
    fetchMock.mockResolvedValueOnce({ ok: true, body: next.body });
    const again = openSessions((value) => heard.push(value));
    await settle();
    next.send(listing("a", "b"));
    await settle();
    again.close();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(heard.at(-1)).toEqual(listing("a", "b"));
  });
});
