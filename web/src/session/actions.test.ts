// resolveGate's contract: success leaves the store to the stream; a 409
// gate_moved folds its corrective detail in immediately; anything else
// rethrows.

import { afterEach, expect, test, vi } from "vitest";
import { ApiError } from "../api/http.ts";
import { recallTurn, resolveGate } from "./actions.ts";
import { pushPendingTurn, Store } from "./store.ts";

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
  const outcome = await resolveGate(store, "a", { decision: "approve" });
  expect(outcome).toBe("moved");
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
  const outcome = await resolveGate(store, "a", { decision: "approve" });
  expect(outcome).toBe("applied");
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

test("recall drops only the echoes the daemon reports popping", async () => {
  const store = new Store();
  store.update((s) => pushPendingTurn(s, "a", "one"));
  store.update((s) => pushPendingTurn(s, "b", "two"));
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      json(200, {
        withdrawn: [{ submission_id: "b", text: "two" }],
      }),
    ),
  );
  expect(await recallTurn(store, "b")).toEqual([
    { submission_id: "b", text: "two" },
  ]);
  expect(store.snapshot().pendingTurns.map((p) => p.id)).toEqual(["a"]);
});

test("a recall that popped nothing leaves the queue alone", async () => {
  const store = new Store();
  store.update((s) => pushPendingTurn(s, "a", "one"));
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(json(200, { withdrawn: [] })),
  );
  expect(await recallTurn(store, "a")).toEqual([]);
  expect(store.snapshot().pendingTurns.map((p) => p.id)).toEqual(["a"]);
});
