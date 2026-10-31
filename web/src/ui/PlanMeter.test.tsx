// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import type { PlanSnapshot, PlanWindow } from "../api/types.ts";
import { PlanMeter } from "./PlanMeter.tsx";
import { elapsedPercent, planMeterOptions, windowLabel } from "./planMeter.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const now = 1_790_000_000_000;
const short: PlanWindow = {
  used_percent: 42,
  window_minutes: 300,
  resets_at: now + 120 * 60_000,
};
const long: PlanWindow = {
  used_percent: 35,
  window_minutes: 10_080,
  resets_at: null,
};
const plan: PlanSnapshot = {
  type: "plus",
  active: null,
  observed_at: now,
  limits: { codex: { name: null, primary: short, secondary: long } },
};

test("elapsed time uses reported duration and reset, with bounded and unknown readings", () => {
  expect(elapsedPercent(short, now)).toBe(60);
  expect(elapsedPercent(short, now - 500 * 60_000)).toBe(0);
  expect(elapsedPercent(short, now + 500 * 60_000)).toBe(100);
  expect(elapsedPercent(long, now)).toBeNull();
  expect(elapsedPercent({ ...short, window_minutes: null }, now)).toBeNull();
  expect(elapsedPercent({ ...short, window_minutes: 0 }, now)).toBeNull();
  expect(windowLabel(10_080, "long")).toBe("week");
  expect(windowLabel(120, "short")).toBe("2h");
  expect(windowLabel(null, "short")).toBe("short");
});

test("only reported windows enter the cycle, with Codex before other allowances", () => {
  expect(planMeterOptions(plan).map((o) => o.key)).toEqual([
    "codex:primary",
    "codex:secondary",
  ]);
  expect(
    planMeterOptions({
      ...plan,
      limits: {
        spark: { name: "Spark", primary: short, secondary: null },
        ...plan.limits,
      },
    }).map((o) => o.key),
  ).toEqual(["codex:primary", "codex:secondary", "spark:primary"]);
  expect(
    renderToStaticMarkup(
      <PlanMeter plan={{ ...plan, limits: {} }} now={now} />,
    ),
  ).toBe("");
});

test("click cycles Codex and reported Spark windows and falls back when an allowance disappears", () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  const withSpark = {
    ...plan,
    limits: {
      ...plan.limits,
      codex_spark: { name: "Spark", primary: short, secondary: long },
    },
  };
  const caption = () => host.querySelector(".context-caption")?.textContent;
  const click = () => act(() => host.querySelector("button")!.click());
  try {
    act(() => root.render(<PlanMeter plan={withSpark} now={now} />));
    expect(caption()).toBe("5h · 42%");
    expect(
      host.querySelector(".plan-meter-time-fill")?.getAttribute("style"),
    ).toContain("60%");
    click();
    expect(caption()).toBe("week · 35%");
    click();
    expect(caption()).toBe("Spark 5h · 42%");
    click();
    expect(caption()).toBe("Spark week · 35%");
    click();
    expect(caption()).toBe("5h · 42%");
    click();
    click();
    act(() => root.render(<PlanMeter plan={plan} now={now} />));
    expect(caption()).toBe("5h · 42%");
    click();
    expect(caption()).toBe("week · 35%");
    expect(host.querySelector(".plan-meter-time-fill")).toBeNull();
  } finally {
    act(() => root.unmount());
  }
});

test("active allowance is initially shown and past resets retain the last reading rather than inventing renewed quota", () => {
  const active: PlanSnapshot = {
    ...plan,
    active: "codex_spark",
    limits: {
      ...plan.limits,
      codex_spark: {
        name: "Spark",
        primary: { ...short, used_percent: 110, resets_at: now - 1 },
        secondary: null,
      },
    },
  };
  const html = renderToStaticMarkup(<PlanMeter plan={active} now={now} />);
  expect(html).toContain("Spark 5h · 100%");
  expect(html).toContain("Allowance used: 100% at last report");
  expect(html).toContain("Window elapsed: 100%");
  expect(html).toContain('tense="auto"');
  expect(html).toContain(`datetime="${new Date(now).toISOString()}"`);
  expect(html).toContain("awaiting usage update");
  expect(html).toContain("aria-describedby=");
});
