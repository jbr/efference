import { afterEach, expect, it, vi } from "vitest";
import { connect } from "./connect.ts";
import { Store } from "./store.ts";

const base = "/sessions/test";
const connectors: ReturnType<typeof connect>[] = [];
afterEach(() => {
  connectors.splice(0).forEach((connector) => connector.close());
  vi.unstubAllGlobals();
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
      code: status === 404 ? "not_found" : "internal",
      message: "test failure",
    },
    { status },
  );

it.each([404, 401])(
  "parks catch-up on HTTP %s without opening a stream",
  async (status) => {
    const { store, fetch } = start(failed(status));
    await vi.waitFor(() =>
      expect(store.snapshot().connection).toBe(
        status === 404 ? "missing" : "unauthenticated",
      ),
    );
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalledWith(
      `${base}/v1/log?tail=200`,
      expect.anything(),
    );
  },
);

it.each([404, 401])(
  "parks when the events request returns HTTP %s",
  async (status) => {
    const { store, fetch } = start(Response.json([]), failed(status));
    await vi.waitFor(() =>
      expect(store.snapshot().connection).toBe(
        status === 404 ? "missing" : "unauthenticated",
      ),
    );
    expect(fetch).toHaveBeenCalledTimes(2);
  },
);

it.each([403, 500])(
  "keeps HTTP %s recoverable rather than declaring absence",
  async (status) => {
    const { store } = start(failed(status), failed(status));
    await vi.waitFor(() =>
      expect(store.snapshot().connection).toBe("reconnecting"),
    );
  },
);

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
