import type { PlanSnapshot, PlanWindow } from "../api/types.ts";

export interface PlanMeterOption {
  key: string;
  allowance: string;
  label: string;
  window: PlanWindow;
}

export function windowLabel(minutes: number | null, kind: string): string {
  if (minutes === null || minutes <= 0) return kind;
  if (minutes % 10_080 === 0)
    return minutes === 10_080 ? "week" : `${minutes / 10_080}w`;
  if (minutes % 1440 === 0) return `${minutes / 1440}d`;
  if (minutes % 60 === 0) return `${minutes / 60}h`;
  return `${minutes}m`;
}

/** Only reported allowances and windows enter the cycle (docs/web-client.md). */
export function planMeterOptions(plan: PlanSnapshot): PlanMeterOption[] {
  return Object.entries(plan.limits)
    .sort(([a], [b]) =>
      a === "codex" ? -1 : b === "codex" ? 1 : a.localeCompare(b),
    )
    .flatMap(([id, limit]) => {
      const allowance =
        limit.name ??
        (id === "codex"
          ? "Codex"
          : id === "codex_spark"
            ? "Spark"
            : id.replaceAll("_", " "));
      return (["primary", "secondary"] as const).flatMap((kind) => {
        const window = limit[kind];
        return window === null
          ? []
          : [
              {
                key: `${id}:${kind}`,
                allowance,
                label: windowLabel(
                  window.window_minutes,
                  kind === "primary" ? "short" : "long",
                ),
                window,
              },
            ];
      });
    });
}

export function elapsedPercent(window: PlanWindow, now: number): number | null {
  if (
    window.resets_at === null ||
    window.window_minutes === null ||
    window.window_minutes <= 0
  )
    return null;
  return Math.max(
    0,
    Math.min(
      100,
      100 * (1 - (window.resets_at - now) / (window.window_minutes * 60_000)),
    ),
  );
}
