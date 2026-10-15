import { useState } from "react";
import { UpstreamNotice } from "../ui/TopBar.tsx";
import { upstreamChip } from "../ui/upstream.ts";
import type { UpstreamFailure, UpstreamRetry } from "../api/types.ts";

const scenarios: {
  label: string;
  failure: UpstreamFailure | null;
  retry: UpstreamRetry | null;
}[] = [
  {
    label: "Overloaded · retrying",
    failure: {
      status: 503,
      count: 5,
      detail:
        '{"error":{"message":"The backend is temporarily overloaded. Please retry.","code":"service_overloaded"}}',
    },
    retry: {
      surface: "turn",
      attempt: 5,
      budget: 12,
      next_attempt_at: 0,
      message: "retrying",
      detail: "overloaded",
    },
  },
  {
    label: "Usage limit",
    failure: {
      status: 429,
      count: 1,
      detail:
        '{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached. Try again after your allowance resets."}}',
    },
    retry: null,
  },
  {
    label: "Stream failed",
    failure: {
      status: null,
      count: 1,
      detail: "The response stream closed before completion.",
    },
    retry: null,
  },
  { label: "Recovered · HTTP 200", failure: null, retry: null },
];

export function UpstreamSpecimens() {
  const [index, setIndex] = useState(0);
  const scenario = scenarios[index];
  const chip = upstreamChip(scenario.failure, scenario.retry);
  return (
    <div>
      <div className="g-row" style={{ flexWrap: "wrap" }}>
        {scenarios.map((item, i) => (
          <button
            key={item.label}
            onClick={() => setIndex(i)}
            aria-pressed={index === i}
          >
            {item.label}
          </button>
        ))}
      </div>
      {chip === null ? (
        <p className="g-label">Recovered — no transport notice</p>
      ) : (
        <UpstreamNotice chip={chip} />
      )}
    </div>
  );
}
