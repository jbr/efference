// The strip under the header (docs/web-client.md): the operator dials —
// interrupt, pace + hold, review level — and the ambient furniture beside
// them: the usage line, one row per process (elapsed, mute, background
// handoff, kill), pending notifications with dismiss. Control furniture,
// not transcript content.

import { useEffect, useState } from "react";
import {
  dismissNotification,
  interruptLoop,
  killProcess,
  patchLoop,
  patchProcess,
} from "../api/http.ts";
import type {
  Pace,
  PendingNotification,
  Process,
  ReviewLevel,
  Usage,
} from "../api/types.ts";

const PACES: [number, string][] = [
  [0, "full"],
  [2000, "2s"],
  [10000, "10s"],
  [30000, "30s"],
];

const LEVELS: ReviewLevel[] = ["autonomous", "spotter_human", "human_only"];

/** Re-render once a second while `active`, for the elapsed clocks. */
function useTick(active: boolean) {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [active]);
}

const elapsed = (spawnedAt: number) => {
  const s = Math.max(0, Math.floor((Date.now() - spawnedAt) / 1000));
  const m = Math.floor(s / 60);
  return m >= 60
    ? `${Math.floor(m / 60)}h${(m % 60).toString().padStart(2, "0")}m`
    : `${m}:${(s % 60).toString().padStart(2, "0")}`;
};

const tokens = (n: number) =>
  n >= 1000 ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k` : `${n}`;

/** Context occupancy + session cost, whichever halves the provider filled. */
function usageLine(usage: Usage): string {
  const parts: string[] = [];
  if (usage.context) {
    const used = usage.context.used === null ? "?" : tokens(usage.context.used);
    parts.push(`ctx ${used}/${tokens(usage.context.limit)}`);
  }
  if (usage.cost) {
    const unpriced =
      usage.cost.unpriced > 0 ? ` (+${usage.cost.unpriced} unpriced)` : "";
    parts.push(`$${usage.cost.usd.toFixed(4)}${unpriced}`);
  }
  return parts.join(" · ");
}

export function Controls({
  pace,
  reviewLevel,
  running,
  processes,
  usage,
  notifications,
}: {
  pace: Pace | null;
  reviewLevel: ReviewLevel | null;
  running: boolean;
  processes: Process[];
  usage: Usage | null;
  notifications: PendingNotification[];
}) {
  const [error, setError] = useState<string | null>(null);
  // One note draft suffices: only the single foreground process offers it.
  const [note, setNote] = useState("");
  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));
  useTick(processes.length > 0);

  const speed = pace?.speed_ms ?? 0;
  return (
    <div className="controls-strip dim">
      {error && <div className="error">{error}</div>}
      <div className="controls">
        <button disabled={!running} onClick={() => act(interruptLoop())}>
          interrupt
        </button>
        <button
          onClick={() => act(patchLoop({ held: !(pace?.held ?? false) }))}
        >
          {pace?.held ? "resume" : "hold"}
        </button>
        <label>
          pace{" "}
          <select
            value={speed}
            onChange={(e) =>
              act(patchLoop({ speedMs: Number(e.target.value) }))
            }
          >
            {!PACES.some(([ms]) => ms === speed) && (
              <option value={speed}>{speed} ms</option>
            )}
            {PACES.map(([ms, label]) => (
              <option key={ms} value={ms}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label>
          review{" "}
          <select
            value={reviewLevel ?? ""}
            onChange={(e) =>
              act(patchLoop({ reviewLevel: e.target.value as ReviewLevel }))
            }
          >
            {reviewLevel === null && <option value="" disabled hidden />}
            {LEVELS.map((level) => (
              <option key={level} value={level}>
                {level}
              </option>
            ))}
          </select>
        </label>
        {usage && <span>{usageLine(usage)}</span>}
      </div>
      {processes.map((proc) => (
        <div className="controls" key={proc.id}>
          <span>
            {proc.background ? "bg" : "fg"} {proc.id}: {proc.command} ·{" "}
            {elapsed(proc.spawned_at)}
            {proc.muted ? " · muted" : ""}
          </span>
          <button
            onClick={() => act(patchProcess(proc.id, { muted: !proc.muted }))}
          >
            {proc.muted ? "unmute" : "mute"}
          </button>
          {!proc.background && (
            <>
              <input
                value={note}
                onChange={(e) => setNote(e.target.value)}
                placeholder="note to ride the handoff (optional)"
              />
              <button
                onClick={() =>
                  act(
                    patchProcess(proc.id, {
                      background: true,
                      note: note.trim() || undefined,
                    }),
                  )
                }
              >
                background
              </button>
            </>
          )}
          <button onClick={() => act(killProcess(proc.id))}>kill</button>
        </div>
      ))}
      {notifications.map((n) => (
        <div className="controls" key={n.id}>
          <span>
            pending — {n.source}
            {n.slot ? ` [${n.slot}]` : ""}: {n.body}
          </span>
          <button onClick={() => act(dismissNotification(n.id))}>
            dismiss
          </button>
        </div>
      ))}
    </div>
  );
}
