import { afterEach, describe, expect, it, vi } from "vitest";
import type { LogEntry } from "../api/entry.ts";
import {
  acquireStore,
  forgetStore,
  prefetchStore,
  WARM_SESSIONS,
} from "./registry.ts";

describe("the warm store registry", () => {
  it("hands back the same store for the same session", () => {
    const store = acquireStore("/sessions/a");
    expect(acquireStore("/sessions/a")).toBe(store);
    forgetStore("/sessions/a");
    expect(acquireStore("/sessions/a")).not.toBe(store);
  });

  it("drops the least recently acquired beyond its capacity", () => {
    const held = Array.from({ length: WARM_SESSIONS }, (_, n) =>
      acquireStore(`/sessions/s${n}`),
    );
    // Touching the first makes the second the oldest.
    expect(acquireStore("/sessions/s0")).toBe(held[0]);
    acquireStore("/sessions/overflow");
    expect(acquireStore("/sessions/s0")).toBe(held[0]);
    expect(acquireStore("/sessions/s1")).not.toBe(held[1]);
  });
});

describe("reading a session ahead of a visit", () => {
  const entry = (seq: number): LogEntry => ({
    seq,
    turn: 1,
    at: 1754300000000,
    payload: { User: [{ Text: `m${seq}` }] },
  });
  const answer = (entries: LogEntry[]) => {
    const fetched = vi.fn<(url: string) => Promise<Response>>(async () =>
      Response.json(entries),
    );
    vi.stubGlobal("fetch", fetched);
    return fetched;
  };
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
  afterEach(() => {
    vi.unstubAllGlobals();
    forgetStore("/sessions/ahead");
  });

  it("fills the store a later visit acquires, from one read of that session", async () => {
    const fetched = answer([entry(1), entry(2)]);
    prefetchStore("/sessions/ahead");
    prefetchStore("/sessions/ahead");
    await settle();
    expect(fetched).toHaveBeenCalledTimes(1);
    expect(fetched.mock.calls[0]![0]).toContain("/sessions/ahead/v1/log?tail=");
    expect(acquireStore("/sessions/ahead").snapshot().lastSeq).toBe(2);
  });

  it("does not move back the cursor of a store a visit has already advanced", async () => {
    answer([entry(1), entry(2)]);
    prefetchStore("/sessions/ahead");
    const store = acquireStore("/sessions/ahead");
    store.update((s) => ({ ...s, lastSeq: 9 }));
    await settle();
    expect(store.snapshot().lastSeq).toBe(9);
  });
});
