// resolveGate's contract: success leaves the store to the stream; a 409
// gate_moved folds its corrective detail in immediately; anything else
// rethrows.

import { afterEach, expect, test, vi } from "vitest";
import { ApiError } from "../api/http.ts";
import { resolveGate } from "./actions.ts";
import { Store } from "./store.ts";

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status });

afterEach(() => vi.unstubAllGlobals());

test("gate_moved applies the corrective gate, no second request", async () => {
  const store = new Store();
  const next = { call_id: "b", name: "bash", input: { command: "ls" } };
  const fetch = vi
    .fn()
    .mockResolvedValue(
      json(409, { code: "gate_moved", message: "moved", detail: next }),
    );
  vi.stubGlobal("fetch", fetch);
  await resolveGate(store, "a", { decision: "approve" });
  expect(store.snapshot().topics.gate).toEqual(next);
  expect(fetch).toHaveBeenCalledTimes(1);
});

test("gate_moved with null detail clears the topic", async () => {
  const store = new Store();
  store.update((s) => ({
    ...s,
    topics: { gate: { call_id: "a", name: "bash", input: {} } },
  }));
  vi.stubGlobal(
    "fetch",
    vi
      .fn()
      .mockResolvedValue(
        json(409, { code: "gate_moved", message: "moved", detail: null }),
      ),
  );
  await resolveGate(store, "a", { decision: "approve" });
  expect(store.snapshot().topics.gate).toBeNull();
});

test("success changes nothing — the stream owns the topic", async () => {
  const store = new Store();
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(json(200, { status: "resolved" })),
  );
  await resolveGate(store, "a", { decision: "approve" });
  expect(store.snapshot().topics.gate).toBeUndefined();
});

test("other errors rethrow as ApiError", async () => {
  const store = new Store();
  vi.stubGlobal(
    "fetch",
    vi
      .fn()
      .mockResolvedValue(
        json(400, { code: "bad_request", message: "bad decision body" }),
      ),
  );
  await expect(
    resolveGate(store, "a", { decision: "approve" }),
  ).rejects.toBeInstanceOf(ApiError);
});
