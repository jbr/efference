import { afterEach, expect, test, vi } from "vitest";
import { ApiError, continueLoop } from "./http.ts";

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
