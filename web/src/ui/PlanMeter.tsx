import { useEffect, useId, useState } from "react";
import type { PlanSnapshot } from "../api/types.ts";
import { HoverPopup } from "./HoverPopup.tsx";
import { ContextBar } from "./ContextBar.tsx";
import { RelativeTime } from "./RelativeTime.tsx";
import { elapsedPercent, planMeterOptions } from "./planMeter.ts";
import "./PlanMeter.css";

/** Subscription occupancy and elapsed wall time (docs/web-client.md). */
export function PlanMeter({
  plan,
  now: fixedNow,
}: {
  plan: PlanSnapshot;
  now?: number;
}) {
  const options = planMeterOptions(plan);
  const [selected, setSelected] = useState<string | null>(null);
  const [clock, setClock] = useState(() => Date.now());
  const detailsId = useId();
  useEffect(() => {
    if (fixedNow !== undefined) return;
    const timer = window.setInterval(() => setClock(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, [fixedNow]);
  const initial = options.findIndex((option) =>
    option.key.startsWith(`${plan.active}:`),
  );
  const selectedIndex = options.findIndex((option) => option.key === selected);
  const index = selectedIndex >= 0 ? selectedIndex : Math.max(0, initial);
  const option = options[index];
  if (!option) return null;
  const now = fixedNow ?? clock;
  const elapsed = elapsedPercent(option.window, now);
  const used = Math.max(0, Math.min(100, option.window.used_percent));
  const expired =
    option.window.resets_at !== null && option.window.resets_at <= now;
  const next = options[(index + 1) % options.length];
  const caption = `${option.key.startsWith("codex:") ? "" : `${option.allowance} `}${option.label} · ${Math.round(used)}%`;
  return (
    <HoverPopup
      id={detailsId}
      role="tooltip"
      className="plan-meter-details"
      renderTrigger={() => (
        <button
          type="button"
          className="plan-meter-trigger"
          aria-label={`${option.allowance} allowance: ${caption}${options.length > 1 ? `. Show ${next.allowance} ${next.label}` : ""}`}
          aria-describedby={detailsId}
          onClick={() => setSelected(next.key)}
        >
          <span className="context-meter plan-meter">
            <span className="plan-meter-track" aria-hidden="true">
              <ContextBar used={used} limit={100} />
              <span className="plan-meter-time-track">
                {elapsed !== null && (
                  <span
                    className="plan-meter-time-fill"
                    style={{ width: `${elapsed}%` }}
                  />
                )}
              </span>
            </span>
            <span className="context-caption">{caption}</span>
          </span>
        </button>
      )}
    >
      <strong className="hover-popup-title">
        {option.allowance} · {option.label} allowance
      </strong>
      <span>
        Allowance used: {Math.round(used)}%{expired ? " at last report" : ""}
      </span>
      <span>
        Window elapsed:{" "}
        {elapsed === null ? "unknown" : `${Math.round(elapsed)}%`}
      </span>
      <span>
        {option.window.resets_at === null ? (
          "Reset time unknown"
        ) : (
          <>
            {expired ? "Reset " : "Resets "}
            <RelativeTime date={option.window.resets_at} tense="auto" />
            {expired && "; awaiting usage update"}
          </>
        )}
      </span>
      <span>
        Reported <RelativeTime date={plan.observed_at} />
      </span>
      {options.length > 1 && (
        <span>
          Click to show {next.allowance} · {next.label}
        </span>
      )}
    </HoverPopup>
  );
}
