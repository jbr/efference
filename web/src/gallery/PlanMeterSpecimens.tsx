import { PlanMeter } from "../ui/PlanMeter.tsx";
import { ContextMeter } from "../ui/TopBar.tsx";
import type { PlanSnapshot } from "../api/types.ts";

const NOW = Date.now();
function specimenPlan(
  used: number,
  elapsed: number | null,
  spark = false,
): PlanSnapshot {
  const window = (minutes: number, percent: number) => ({
    used_percent: percent,
    window_minutes: minutes,
    resets_at:
      elapsed === null ? null : NOW + minutes * 60_000 * (1 - elapsed / 100),
  });
  return {
    type: "plus",
    active: null,
    observed_at: NOW - 60_000,
    limits: {
      codex: {
        name: null,
        primary: window(300, used),
        secondary: window(10_080, 35),
      },
      ...(spark
        ? {
            codex_spark: {
              name: "Spark",
              primary: window(300, 15),
              secondary: window(10_080, 65),
            },
          }
        : {}),
    },
  };
}

export function PlanMeterSpecimens() {
  return (
    <>
      <p className="g-intro">
        Top fill: allowance used. Bottom line: wall time elapsed. Click to cycle
        through reported windows; hover or focus for the readings and reset
        time. Only usage glows. These fixtures have a fixed clock.
      </p>
      <div className="g-contexts">
        {[
          [0, 0, "Fresh window"],
          [20, 60, "Usage behind time"],
          [50, 50, "Equal progress"],
          [75, 25, "Usage ahead of time"],
          [90, 80, "Near limit"],
          [100, 40, "Exhausted"],
          [42, null, "Timing unknown"],
          [100, 110, "Reset passed"],
        ].map(([used, elapsed, label]) => (
          <figure className="g-state" key={label}>
            <PlanMeter
              plan={specimenPlan(used as number, elapsed as number | null)}
              now={NOW}
            />
            <figcaption>{label}</figcaption>
          </figure>
        ))}
      </div>
      <p className="g-intro">
        Header scale: context followed by plan allowance. Left has only Codex
        windows; right also reports Spark. Spark is not added when absent from
        telemetry.
      </p>
      <div className="g-contexts" style={{ fontSize: "0.8rem" }}>
        {[false, true].map((spark) => (
          <figure className="g-state" key={String(spark)}>
            <div
              style={{ display: "flex", alignItems: "center", gap: "0.8rem" }}
            >
              <ContextMeter context={{ used: 100_000, limit: 200_000 }} />
              <PlanMeter plan={specimenPlan(42, 60, spark)} now={NOW} />
            </div>
            <figcaption>
              {spark ? "Codex + Spark cycle" : "Codex cycle"}
            </figcaption>
          </figure>
        ))}
      </div>
    </>
  );
}
