import { expect, test } from "vitest";
import type { LogEntry } from "../api/entry.ts";
import { emptyStreaming } from "./store.ts";
import { summaryPreviews } from "./summaryPreviews.ts";

test("retains the recorded compaction cut after operations has disappeared", () => {
  const entries: LogEntry[] = [
    {
      seq: 8,
      turn: 1,
      at: 0,
      payload: {
        Telemetry: { CompactionRequested: { warm: true, cut: 5 } },
      },
    },
  ];
  const streaming = {
    ...emptyStreaming,
    compaction: { text: "account", thinking: "check" },
    consolidation: { from: 2, upto: 4, text: "earlier", thinking: "span" },
  };
  expect(summaryPreviews(entries, streaming, null)).toEqual([
    {
      kind: "summary-preview",
      key: "live-compaction",
      source: "compaction",
      from: 0,
      upto: 5,
      text: "account",
      thinking: "check",
    },
    {
      kind: "summary-preview",
      key: "live-consolidation",
      source: "consolidation",
      from: 2,
      upto: 4,
      text: "earlier",
      thinking: "span",
    },
  ]);
});

test("does not invent a span for an unanchored compaction tail", () => {
  expect(
    summaryPreviews(
      [],
      { ...emptyStreaming, compaction: { text: "account", thinking: "" } },
      null,
    ),
  ).toEqual([]);
});
