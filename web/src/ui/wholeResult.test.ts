// The dereference half of a truncated result (docs/control-api.md): the entry
// route serves the same entry whole, so the client reads its result out of it.

import { expect, test } from "vitest";
import type { LogEntry } from "../api/entry.ts";
import { resultOf } from "./wholeResult.ts";

const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 1754300000000,
  payload,
});

test("the whole result is read out of the fetched entry", () => {
  const fetched = entry(42, {
    ToolResult: {
      call_id: "toolu_1",
      raw: { content: "the whole thing", is_error: false },
      wire: "Raw",
    },
  });
  expect(resultOf(fetched)).toEqual({
    content: "the whole thing",
    is_error: false,
  });
  expect(resultOf(fetched)?.content_elided).toBeUndefined();
});

test("an entry that is not a tool result yields nothing to substitute", () => {
  expect(resultOf(entry(41, { User: [{ Text: "hi" }] }))).toBeNull();
});
