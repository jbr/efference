import { afterEach, expect, it, vi } from "vitest";
import type { LogEntry } from "../api/entry.ts";
import type { LocatedToolCall } from "../api/types.ts";
import { fetchCallSeq, fetchLogBefore, fetchLogRange } from "../api/http.ts";
import { messagesAssistant } from "./assistant.ts";
import {
  coherentWindow,
  HISTORY_PAGE,
  HistoryLoader,
  prependHistory,
} from "./history.ts";
import { applyFrame, emptySession, seed, Store } from "./store.ts";
import { buildTree } from "./tree.ts";
vi.mock("../api/http.ts", () => ({
  fetchCallSeq: vi.fn(),
  fetchLogBefore: vi.fn(),
  fetchLogRange: vi.fn(),
}));
afterEach(() => vi.resetAllMocks());
const entry = (
  seq: number,
  payload: LogEntry["payload"] = { User: [{ Text: `${seq}` }] },
): LogEntry => ({ seq, turn: 1, at: 0, payload });
const call = (seq: number, id = "c") =>
  entry(seq, {
    Assistant: messagesAssistant([], [], [{ id, name: "read", input: {} }]),
  });
/** What the host answers a call's address with: the entry's seq, and the call. */
const located = (seq: number, id = "c"): LocatedToolCall => ({
  seq,
  call: { id, name: "read", input: {} },
});
const result = (seq: number, id = "c") =>
  entry(seq, {
    ToolResult: {
      call_id: id,
      raw: { content: "ok", is_error: false },
      wire: "Raw",
    },
  });
function setup() {
  const store = new Store((flush) => flush());
  store.update((s) => seed(s, [entry(100), entry(101)]));
  return { store, loader: new HistoryLoader(store, "/sessions/source") };
}
it("merges sorted history without changing live state or duplicating held entries", () => {
  const s = seed(emptySession, [entry(100), entry(101)]);
  const older = prependHistory(s, [entry(101), entry(99), entry(98)]);
  expect(older.entries.map((e) => e.seq)).toEqual([98, 99, 100, 101]);
  expect(older.entries.at(-1)).toBe(s.entries.at(-1));
  expect(older.lastSeq).toBe(101);
  expect(older.topics).toBe(s.topics);
  expect(older.streaming).toBe(s.streaming);
  expect(older.pendingTurns).toBe(s.pendingTurns);
  expect(prependHistory(older, [entry(98)])).toBe(older);
});
it("binds the page to its session and retains frames arriving during the read", async () => {
  const { store, loader } = setup();
  let resolve!: (entries: LogEntry[]) => void;
  vi.mocked(fetchLogBefore).mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const beforeMerge = vi.fn();
  const request = loader.load(beforeMerge);
  await loader.load();
  expect(fetchLogBefore).toHaveBeenCalledTimes(1);
  expect(fetchLogBefore).toHaveBeenCalledWith(
    100,
    HISTORY_PAGE,
    "/sessions/source",
  );
  store.update((s) =>
    applyFrame(s, { class: "log", kind: "user", seq: 102, data: entry(102) }),
  );
  resolve([entry(98), entry(99)]);
  await request;
  expect(beforeMerge).toHaveBeenCalledOnce();
  expect(store.snapshot().entries.map((e) => e.seq)).toEqual([
    98, 99, 100, 101, 102,
  ]);
  expect(store.snapshot().lastSeq).toBe(102);
  expect(loader.snapshot()).toEqual({
    loading: false,
    done: true,
    error: null,
  });
});
it("discards a page when a reseed changed the loaded window", async () => {
  const { store, loader } = setup();
  let resolve!: (entries: LogEntry[]) => void;
  vi.mocked(fetchLogBefore).mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const capture = vi.fn();
  const request = loader.load(capture);
  store.update((s) => seed(s, [entry(300)]));
  resolve([entry(99)]);
  await request;
  expect(store.snapshot().entries.map((e) => e.seq)).toEqual([300]);
  expect(capture).not.toHaveBeenCalled();
  expect(loader.snapshot().done).toBe(false);
});
it("exposes an error without changing the transcript and allows retry", async () => {
  const { store, loader } = setup();
  const held = store.snapshot();
  vi.mocked(fetchLogBefore)
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValueOnce([]);
  await loader.load();
  expect(store.snapshot()).toBe(held);
  expect(loader.snapshot().error).toBe("offline");
  await loader.load();
  expect(loader.snapshot()).toEqual({
    loading: false,
    done: true,
    error: null,
  });
});
it("keeps paging on an exactly full page and stops on the short final page", async () => {
  const { loader } = setup();
  vi.mocked(fetchLogBefore)
    .mockResolvedValueOnce(
      Array.from({ length: HISTORY_PAGE }, (_, i) => entry(20 + i)),
    )
    .mockResolvedValueOnce([entry(19)]);
  await loader.load();
  expect(loader.snapshot().done).toBe(false);
  await loader.load();
  expect(fetchLogBefore).toHaveBeenLastCalledWith(
    20,
    HISTORY_PAGE,
    "/sessions/source",
  );
  expect(loader.snapshot().done).toBe(true);
});
it("repairs a cut through a tool result without skipping intervening entries", async () => {
  vi.mocked(fetchCallSeq).mockResolvedValue(located(5));
  vi.mocked(fetchLogRange).mockResolvedValue([call(5), entry(6)]);
  const entries = await coherentWindow(
    [result(7), entry(8)],
    "/sessions/source",
  );
  expect(fetchLogRange).toHaveBeenCalledWith(5, 7, "/sessions/source");
  expect(entries.map((e) => e.seq)).toEqual([5, 6, 7, 8]);
  const tree = buildTree(entries);
  expect(tree[0].kind).toBe("batch");
  if (tree[0].kind === "batch") expect(tree[0].calls[0].result?.seq).toBe(7);
});
it("does not fetch references already contained in a window", async () => {
  const entries = [call(5), result(7)];
  expect(await coherentWindow(entries, "base")).toBe(entries);
  expect(fetchCallSeq).not.toHaveBeenCalled();
});
it("fails rather than looping when an earlier reference cannot be resolved", async () => {
  vi.mocked(fetchCallSeq).mockResolvedValue(located(5));
  vi.mocked(fetchLogRange).mockResolvedValue([]);
  await expect(coherentWindow([result(7)], "base")).rejects.toThrow(
    "references",
  );
});

it("repairs a verdict-to-result-to-call chain across successive cuts", async () => {
  const pending = result(7);
  if ("ToolResult" in pending.payload)
    pending.payload.ToolResult.wire = "Pending";
  vi.mocked(fetchLogRange)
    .mockResolvedValueOnce([pending])
    .mockResolvedValueOnce([call(5), entry(6)]);
  vi.mocked(fetchCallSeq).mockResolvedValue(located(5));
  const entries = await coherentWindow(
    [entry(8, { ResultVerdict: { for_result: 7, wire: "Raw" } })],
    "base",
  );
  expect(entries.map((e) => e.seq)).toEqual([5, 6, 7, 8]);
  const node = buildTree(entries)[0];
  expect(node.kind).toBe("batch");
  if (node.kind === "batch")
    expect(node.calls[0].result?.payload).toMatchObject({
      ToolResult: { wire: "Raw" },
    });
});

it("clears exhausted history when a disjoint newer seed replaces the window", async () => {
  const { store, loader } = setup();
  vi.mocked(fetchLogBefore).mockResolvedValue([]);
  await loader.load();
  expect(loader.snapshot().done).toBe(true);
  store.update((s) => seed(s, [entry(300)]));
  expect(loader.snapshot().done).toBe(false);
  await loader.load();
  expect(fetchLogBefore).toHaveBeenLastCalledWith(
    300,
    HISTORY_PAGE,
    "/sessions/source",
  );
});
