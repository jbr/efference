import { afterEach, expect, it, vi } from "vitest";
import { bearer } from "../api/bearer.ts";
import { connect } from "./connect.ts";
import { Store } from "./store.ts";

const base = "/sessions/test";
const connectors: ReturnType<typeof connect>[] = [];
afterEach(async () => {
  connectors.splice(0).forEach((connector) => connector.close());
  vi.unstubAllGlobals();
  await bearer.clear();
});

function start(...responses: Response[]) {
  const fetch = vi.fn();
  for (const response of responses) fetch.mockResolvedValueOnce(response);
  vi.stubGlobal("fetch", fetch);
  const store = new Store((flush) => flush());
  connectors.push(connect(store, undefined, base));
  return { store, fetch };
}

const failed = (status: number) =>
  Response.json(
    {
      code:
        status === 404
          ? "not_found"
          : status === 401
            ? "unauthorized"
            : "internal",
      message: "test failure",
    },
    { status },
  );

// A 404 is about the session, not the credential: nothing is behind it to
// reconnect to, and the held bearer stays.
it("parks catch-up on HTTP 404 without opening a stream", async () => {
  await bearer.store("held");
  const { store, fetch } = start(failed(404));
  await vi.waitFor(() => expect(store.snapshot().connection).toBe("missing"));
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch).toHaveBeenCalledWith(
    `${base}/v1/log?tail=80`,
    expect.anything(),
  );
  expect(bearer.held()).toBe("held");
});

it("parks when the events request returns HTTP 404", async () => {
  await bearer.store("held");
  const { store, fetch } = start(Response.json([]), failed(404));
  await vi.waitFor(() => expect(store.snapshot().connection).toBe("missing"));
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(bearer.held()).toBe("held");
});

// A 401 is dropped where it is refused (`apiFetch`) and the app closes the
// connection (ui/App.tsx); until then it is retried like any other refusal.
it.each([401, 403, 500])(
  "keeps HTTP %s recoverable rather than declaring absence",
  async (status) => {
    const { store } = start(failed(status), failed(status));
    await vi.waitFor(() =>
      expect(store.snapshot().connection).toBe("reconnecting"),
    );
  },
);

it("retries after a 401 for a bearer since replaced, carrying the new one", async () => {
  vi.stubGlobal("location", new URL("http://localhost:7777/"));
  await bearer.store("old");
  let refuse!: (response: Response) => void;
  const fetch = vi
    .fn()
    .mockReturnValueOnce(
      new Promise<Response>((resolve) => {
        refuse = resolve;
      }),
    )
    .mockResolvedValueOnce(new Response(new ReadableStream()));
  vi.stubGlobal("fetch", fetch);
  const store = new Store((flush) => flush());
  connectors.push(connect(store, undefined, base));

  // A sign-in lands while the catch-up read is out; its refusal comes after.
  await bearer.store("new");
  refuse(failed(401));

  await vi.waitFor(() => expect(store.snapshot().connection).toBe("live"));
  expect(bearer.held()).toBe("new");
  const sent = (call: number) =>
    new Headers((fetch.mock.calls[call][1] as RequestInit).headers).get(
      "authorization",
    );
  expect(sent(0)).toBe("Bearer old");
  expect(sent(1)).toBe("Bearer new");
});

it("keeps network failure recoverable", async () => {
  vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
  const store = new Store((flush) => flush());
  connectors.push(connect(store, undefined, base));
  await vi.waitFor(() =>
    expect(store.snapshot().connection).toBe("reconnecting"),
  );
});

it("detects external archive when the live stream ends and reconnect is missing", async () => {
  vi.useFakeTimers();
  let end!: () => void;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      end = () => controller.close();
    },
  });
  const { store, fetch } = start(
    Response.json([]),
    new Response(body),
    failed(404),
  );
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(store.snapshot().connection).toBe("live");
    end();
    await vi.advanceTimersByTimeAsync(0);
    expect(store.snapshot().connection).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1000);
    expect(store.snapshot().connection).toBe("missing");
    await vi.advanceTimersByTimeAsync(30000);
    expect(fetch).toHaveBeenCalledTimes(3);
  } finally {
    vi.useRealTimers();
  }
});

it("retains a fetched tail and opens its stream when a boundary reference is missing", async () => {
  const entry = {
    seq: 50,
    turn: 1,
    at: 0,
    payload: {
      ToolResult: {
        call_id: "absent",
        raw: { content: "ok", is_error: false },
        wire: "Raw",
      },
    },
  };
  const { store, fetch } = start(
    Response.json([entry]),
    failed(404),
    new Response(new ReadableStream()),
  );
  await vi.waitFor(() => expect(store.snapshot().connection).toBe("live"));
  expect(store.snapshot().lastSeq).toBe(50);
  expect(store.snapshot().entries).toEqual([entry]);
  expect(fetch).toHaveBeenLastCalledWith(
    `${base}/v1/events?since=51`,
    expect.anything(),
  );
});

it("stops at a failed start instead of reconnecting to it", async () => {
  vi.useFakeTimers();
  const frame = {
    class: "state",
    kind: "starting",
    data: {
      id: "test",
      cwd: "/work",
      since: 1,
      resume: false,
      stage: "environment",
      authorizing: null,
      failure: { reason: "env: the hook exited 7", at: 2, withdrawn: [] },
    },
  };
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(
        new TextEncoder().encode(`data: ${JSON.stringify(frame)}\n\n`),
      );
      controller.close();
    },
  });
  const starting = Response.json(
    { code: "starting", message: 'session "test" is starting' },
    { status: 409 },
  );
  const { store, fetch } = start(starting, new Response(body));
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(store.snapshot().connection).toBe("ended");
    expect(store.snapshot().topics.starting?.failure?.reason).toBe(
      "env: the hook exited 7",
    );
    await vi.advanceTimersByTimeAsync(30000);
    expect(fetch).toHaveBeenCalledTimes(2);
  } finally {
    vi.useRealTimers();
  }
});
