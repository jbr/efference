import { expect, test } from "vitest";
import { callView } from "../render/parse.ts";
import { truncateBody, truncateText, TRUNCATE_AT } from "./truncate.ts";

const lines = (n: number) =>
  Array.from({ length: n }, (_, i) => `line ${i}`).join("\n");

test("a write's contents truncate at desktop width, not only on the phone", () => {
  const [body] = callView("write", {
    path: "src/main.rs",
    contents: lines(505),
  }).bodies;
  const shown = truncateText(body.text, false);
  expect(shown.text.split("\n")).toHaveLength(TRUNCATE_AT);
  expect(shown.hidden).toBe(505 - TRUNCATE_AT);
  // The indicator never stands in for fewer lines than it occupies.
  expect(truncateText(lines(TRUNCATE_AT + 2), false).hidden).toBe(0);
  expect(truncateText(lines(505), true).hidden).toBe(501);
});

test("a result truncates for read and write at desktop, every tool on narrow", () => {
  const body = { kind: "text", text: lines(40) } as const;
  expect(truncateBody(body, "read", false).hidden).toBe(28);
  expect(truncateBody(body, "search", false).hidden).toBe(0);
  expect(truncateBody(body, "search", true).hidden).toBe(36);
});
