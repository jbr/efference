import { afterEach, expect, test, vi } from "vitest";
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

afterEach(() => vi.unstubAllGlobals());

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

test("apiFetch passes its arguments to the global fetch as it finds it at the call", async () => {
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
