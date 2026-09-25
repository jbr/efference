// The detail-by-recency rules (docs/web-client.md): batches flatten into
// calls, levels fall with distance from the tail, and what must be seen is
// whole at any distance.

import { expect, test } from "vitest";
import type { LogEntry, WireState } from "../api/types.ts";
import { messagesAssistant } from "../session/assistant.ts";
import { buildTree, type CallNode } from "../session/tree.ts";
import {
  ANCIENT,
  flatten,
  type Item,
  levelFor,
  RECENT,
  wordCount,
} from "./transcriptLevels.ts";

const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 0,
  payload,
});

const issued = (seq: number, ids: string[]) =>
  entry(seq, {
    Assistant: messagesAssistant(
      [],
      [],
      ids.map((id) => ({ id, name: "read", input: {} })),
    ),
  });

const answered = (
  seq: number,
  id: string,
  wire: WireState = "Raw",
  isError = false,
) =>
  entry(seq, {
    ToolResult: {
      call_id: id,
      raw: { content: "ok", is_error: isError },
      wire,
    },
  });

const calls = (items: Item[]): CallNode[] =>
  items.flatMap((item) => (item.kind === "call" ? [item.node] : []));

test("a batch flattens into one item per call, in order", () => {
  const items = flatten(
    buildTree([issued(1, ["a", "b"]), answered(2, "a"), answered(3, "b")]),
  );
  expect(items.map((item) => item.kind)).toEqual(["call", "call"]);
  expect(calls(items).map((call) => call.callId)).toEqual(["a", "b"]);
});

test("detail falls with distance from the tail", () => {
  const [done] = calls(
    flatten(buildTree([issued(1, ["a"]), answered(2, "a")])),
  );
  const item: Item = { kind: "call", node: done };
  expect(levelFor(item, 0)).toBe("full");
  expect(levelFor(item, RECENT - 1)).toBe("full");
  expect(levelFor(item, RECENT)).toBe("trimmed");
  expect(levelFor(item, ANCIENT - 1)).toBe("trimmed");
  expect(levelFor(item, ANCIENT)).toBe("brief");
});

test("running, failed, withheld, and flagged calls are whole at any distance", () => {
  const far = ANCIENT + 10;
  const level = (entries: LogEntry[]) =>
    levelFor(
      { kind: "call", node: calls(flatten(buildTree(entries)))[0] },
      far,
    );
  expect(level([issued(1, ["a"])])).toBe("full");
  expect(level([issued(1, ["a"]), answered(2, "a", "Raw", true)])).toBe("full");
  expect(
    level([
      issued(1, ["a"]),
      answered(2, "a", { TriageFailed: { why: "no verdict" } }),
    ]),
  ).toBe("full");
  expect(
    level([
      issued(1, ["a"]),
      entry(2, {
        Spot: {
          call_id: "a",
          stance: "watch",
          reason: "rm -rf",
          origin: "Model",
        },
      }),
      answered(3, "a"),
    ]),
  ).toBe("full");
  expect(level([issued(1, ["a"]), answered(2, "a")])).toBe("brief");
});

test("words are what the thinking floor counts", () => {
  expect(wordCount("  one two\n\nthree  ")).toBe(3);
});
