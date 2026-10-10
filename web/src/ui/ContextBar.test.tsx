import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ContextBar } from "./ContextBar.tsx";

const bar = (used: number | null, limit = 200_000) =>
  renderToStaticMarkup(<ContextBar used={used} limit={limit} />);

test("context fill is bounded, with no fill for unknown or invalid capacity", () => {
  expect(bar(50_000)).toContain("width:25%");
  expect(bar(300_000)).toContain("width:100%");
  expect(bar(-1)).toContain("width:0%");
  expect(bar(null)).toContain("width:0%");
  expect(bar(10, 0)).toContain("width:0%");
});

test("context light strengthens and warms near capacity", () => {
  expect(bar(20_000)).toContain("--light-level:0.1");
  expect(bar(20_000)).toContain("--meter-warm:0");
  expect(bar(20_000)).toContain("--meter-hot:0");
  expect(bar(200_000)).toContain("--light-level:1");
  expect(bar(200_000)).toContain("--meter-warm:1");
  expect(bar(200_000)).toContain("--meter-hot:1");
});
