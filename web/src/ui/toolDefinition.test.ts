// The memo's two failure answers (mirroring processRecord.ts): a 404 is a fact
// about the session and is cached, a transport failure is this request failing
// and is not.

import { afterEach, expect, test, vi } from "vitest";

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status });

const entry = {
  name: "ferritin__get",
  description: "Show documentation for a Rust item by path.",
  input_schema: { type: "object", properties: {} },
  server: "ferritin",
};

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

/** A fresh module, because the cache is module-level and page-lifetime. */
const fresh = async () => (await import("./toolDefinition.ts")).loadToolEntry;

test("a definition is fetched once and served from the memo after", async () => {
  const fetchMock = vi.fn((url: string) =>
    Promise.resolve(json(200, { ...entry, requested: url })),
  );
  vi.stubGlobal("fetch", fetchMock);
  const loadToolEntry = await fresh();

  // Concurrent askers share one flight, and a later ask costs nothing.
  const [a, b] = await Promise.all([
    loadToolEntry("ferritin__get"),
    loadToolEntry("ferritin__get"),
  ]);
  await loadToolEntry("ferritin__get");

  expect(a?.description).toBe(entry.description);
  expect(b?.name).toBe("ferritin__get");
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(fetchMock.mock.calls[0][0]).toBe("/v1/tools/ferritin__get");
});

test("a 404 is durable; a transport failure is retried", async () => {
  const fetchMock = vi.fn(() =>
    Promise.resolve(json(404, { code: "not_found", message: "no such tool" })),
  );
  vi.stubGlobal("fetch", fetchMock);
  const loadToolEntry = await fresh();

  // No definition to have — the offline-session case — and asking again does
  // not re-ask.
  expect(await loadToolEntry("gone")).toBe(null);
  expect(await loadToolEntry("gone")).toBe(null);
  expect(fetchMock).toHaveBeenCalledTimes(1);

  const flaky = vi
    .fn()
    .mockRejectedValueOnce(new TypeError("network"))
    .mockResolvedValue(json(200, entry));
  vi.stubGlobal("fetch", flaky);
  expect(await loadToolEntry("ferritin__get")).toBe(null);
  expect((await loadToolEntry("ferritin__get"))?.name).toBe("ferritin__get");
  expect(flaky).toHaveBeenCalledTimes(2);
});
