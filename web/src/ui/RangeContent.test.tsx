// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { RangeContent } from "./RangeContent.tsx";
import { mergeRanges } from "./lineRanges.ts";
vi.mock("./highlight.ts", () => ({ useHighlight: () => null }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
test("normalizes overlapping adjacent and out-of-bounds ranges", () => {
  expect(
    mergeRanges(
      [
        [8, 20],
        [3, 5],
        [0, 3],
        [6, 7],
        [30, 32],
      ],
      10,
    ),
  ).toEqual([[1, 10]]);
});
test("expands both sides, merges ranges, and resets for a different captured source", () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  const text = Array.from({ length: 20 }, (_, i) => `line ${i + 1}`).join("\n");
  const retained = [
    [3, 4],
    [12, 13],
  ] as const;
  const numbers = () =>
    [...host.querySelectorAll("[data-line]")].map((e) =>
      Number(e.getAttribute("data-line")),
    );
  try {
    act(() =>
      root.render(<RangeContent text={text} retained={retained} step={4} />),
    );
    expect(numbers()).toEqual([3, 4, 12, 13]);
    const click = (label: string) =>
      act(() => {
        const button = [...host.querySelectorAll("button")].find(
          (e) => e.getAttribute("aria-label") === label,
        );
        expect(button).toBeDefined();
        button!.click();
      });
    click("Show 4 lines after line 4");
    expect(numbers()).toEqual([3, 4, 5, 6, 7, 8, 12, 13]);
    click("Show 3 lines before line 12");
    expect(numbers()).toEqual(Array.from({ length: 11 }, (_, i) => i + 3));
    expect(host.querySelectorAll(".is-retained")).toHaveLength(4);
    expect(new Set(numbers()).size).toBe(numbers().length);
    act(() =>
      root.render(
        <RangeContent text={text + "\nnew"} retained={retained} step={4} />,
      ),
    );
    expect(numbers()).toEqual([3, 4, 12, 13]);
  } finally {
    act(() => root.unmount());
  }
});
