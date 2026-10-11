// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import type { Frame } from "../api/types.ts";
import { record } from "./record.ts";
import { Store } from "./store.ts";
import { loadTape, saveTape } from "./tapeStore.ts";

vi.mock("./tapeStore.ts", () => ({
  saveTape: vi.fn(async () => {}),
  loadTape: vi.fn(),
  listTapes: vi.fn(async () => []),
}));
afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

const delta = (fragment: string): Frame => ({
  class: "output",
  kind: "tool_call_input",
  data: { index: 0, fragment },
});

test("save and restore the latest window with its exact rebased starting state", async () => {
  vi.useFakeTimers();
  const store = new Store();
  const { tap, scrub, close } = record(store, 3);
  try {
    tap.frame({
      class: "output",
      kind: "tool_call_start",
      data: { index: 0, call_id: "c1", name: "write" },
    });
    for (const fragment of ["a", "b", "c", "d", "e"])
      tap.frame(delta(fragment));
    const live = store.snapshot();
    await vi.advanceTimersByTimeAsync(1000);
    const saved = vi.mocked(saveTape).mock.calls[0][0];
    expect(saved.base.streaming.blocks[0].text).toBe("ab");
    expect(saved.tape).toEqual([delta("c"), delta("d"), delta("e")]);
    tap.frame(delta("f"));
    expect(saved.tape).toEqual([delta("c"), delta("d"), delta("e")]);
    vi.mocked(loadTape).mockResolvedValue(saved);
    await scrub.restore("saved");
    expect(store.snapshot()).toEqual(live);
    scrub.seek(0);
    expect(store.snapshot().streaming.blocks[0].text).toBe("ab");
    scrub.seek(3);
    expect(store.snapshot()).toEqual(live);
  } finally {
    close();
  }
});
