import { afterEach, expect, test, vi } from "vitest";
import { bearer } from "./bearer.ts";
import {
  ApiError,
  apiFetch,
  continueLoop,
  fetchLogBefore,
  fetchLogRange,
  fetchCallSeq,
} from "./http.ts";

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status });

afterEach(async () => {
  vi.unstubAllGlobals();
  // The page's own store: outside a browser it has no IndexedDB and holds the
  // bearer in memory alone.
  await bearer.clear();
});

/** A page at `origin` holding `token`, with a fetch stub that records what it
 * was handed. */
async function signedIn(token: string, origin = "http://localhost:7777") {
  vi.stubGlobal("location", new URL(`${origin}/`));
  await bearer.store(token);
  const fetchMock = vi.fn().mockResolvedValue(json(200, {}));
  vi.stubGlobal("fetch", fetchMock);
  /** The headers the stub was handed on its `n`th call. */
  const sent = (n = 0) =>
    new Headers((fetchMock.mock.calls[n][1] as RequestInit).headers);
  return { fetchMock, sent };
}

test("continue posts the loop verb with no payload of its own", async () => {
  const fetchMock = vi
    .fn()
    .mockResolvedValue(json(202, { status: "continuing" }));
  vi.stubGlobal("fetch", fetchMock);

  await continueLoop();

  expect(fetchMock).toHaveBeenCalledTimes(1);
  const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
  expect(path).toBe("/v1/loop/continue");
  expect(init.method).toBe("POST");
});

test("a decline arrives as an ApiError naming the case", async () => {
  // A fresh Response per call: a body can only be read once.
  vi.stubGlobal(
    "fetch",
    vi.fn(() =>
      Promise.resolve(
        json(409, {
          code: "not_continuable",
          message: "nothing to continue: the last turn completed",
        }),
      ),
    ),
  );

  const error = await continueLoop().catch((e: unknown) => e);
  expect(error).toBeInstanceOf(ApiError);
  expect((error as ApiError).code).toBe("not_continuable");
  expect((error as ApiError).status).toBe(409);
  expect((error as ApiError).message).toBe(
    "nothing to continue: the last turn completed",
  );
});

test("history reads bind ranges and encoded references to their source session", async () => {
  const fetchMock = vi
    .fn()
    .mockImplementation(() => Promise.resolve(json(200, [])));
  vi.stubGlobal("fetch", fetchMock);
  await fetchLogBefore(42, 80, "/sessions/source");
  await fetchLogRange(12, 42, "/sessions/source");
  await fetchCallSeq("call/id?", "/sessions/source");
  expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
    "/sessions/source/v1/log?before=42&tail=80",
    "/sessions/source/v1/log?since=12&before=42",
    "/sessions/source/v1/tool_calls/call%2Fid%3F",
  ]);
  for (const [, init] of fetchMock.mock.calls) expect(init.method).toBe("GET");
});

test("with no bearer held, apiFetch passes its arguments to the global fetch as it finds it at the call", async () => {
  const response = json(200, {});
  const first = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", first);

  const init = { method: "POST", headers: { "x-probe": "1" } };
  await expect(apiFetch("/x", init)).resolves.toBe(response);
  expect(first).toHaveBeenCalledExactlyOnceWith("/x", init);

  // No second argument is invented for a call that gave none.
  await apiFetch("/y");
  expect(first).toHaveBeenLastCalledWith("/y");

  // A stub installed later is the one that answers.
  const second = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", second);
  await apiFetch("/z");
  expect(second).toHaveBeenCalledTimes(1);
  expect(first).toHaveBeenCalledTimes(2);
});

test("apiFetch attaches the held bearer, merged into headers in each form fetch takes", async () => {
  const { sent } = await signedIn("tok");
  await apiFetch("/v1/state");
  await apiFetch("/v1/turns", {
    method: "POST",
    headers: { "content-type": "application/json" },
  });
  await apiFetch("/v1/log", { headers: [["accept", "application/json"]] });
  await apiFetch("/v1/events", {
    headers: new Headers({ accept: "text/event-stream" }),
  });

  for (let n = 0; n < 4; n++) {
    expect(sent(n).get("authorization")).toBe("Bearer tok");
  }
  expect(sent(1).get("content-type")).toBe("application/json");
  expect(sent(2).get("accept")).toBe("application/json");
  expect(sent(3).get("accept")).toBe("text/event-stream");
});

test("apiFetch keeps the caller's other init and leaves its Authorization alone", async () => {
  const { fetchMock, sent } = await signedIn("tok");
  const signal = new AbortController().signal;
  await apiFetch("/v1/state", { signal, keepalive: true });
  await apiFetch("/v1/state", { headers: { Authorization: "Bearer theirs" } });
  await apiFetch("/v1/state", {
    headers: new Headers({ authorization: "Basic other" }),
  });

  const init = fetchMock.mock.calls[0][1] as RequestInit;
  expect(init.signal).toBe(signal);
  expect(init.keepalive).toBe(true);
  expect(sent(1).get("authorization")).toBe("Bearer theirs");
  expect(sent(2).get("authorization")).toBe("Basic other");
});

test("apiFetch carries a Request's own headers alongside the bearer", async () => {
  const { fetchMock } = await signedIn("tok");
  await apiFetch(
    new Request("http://localhost:7777/v1/state", {
      headers: { accept: "application/json" },
    }),
  );
  const headers = new Headers(
    (fetchMock.mock.calls[0][1] as RequestInit).headers,
  );
  expect(headers.get("authorization")).toBe("Bearer tok");
  expect(headers.get("accept")).toBe("application/json");
});

test("apiFetch sends the bearer to its own origin only", async () => {
  const { fetchMock } = await signedIn("tok", "http://localhost:7777");
  const init = { headers: { accept: "image/*" } };
  for (const url of [
    "https://elsewhere.example/x.png",
    "http://localhost:3000/v1/state",
    new URL("https://elsewhere.example/y"),
  ]) {
    await apiFetch(url, init);
    expect(fetchMock).toHaveBeenLastCalledWith(url, init);
  }
  await apiFetch("http://localhost:7777/v1/state");
  const sent = fetchMock.mock.calls.at(-1)?.[1] as RequestInit;
  expect(new Headers(sent.headers).get("authorization")).toBe("Bearer tok");
});
