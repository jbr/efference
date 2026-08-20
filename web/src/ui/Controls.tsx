// The strip under the header (docs/web-client.md): the ambient furniture —
// the usage line, one row per process (elapsed, mute, background handoff,
// kill), pending notifications with dismiss. The loop dials moved to the
// composer's status row (that is where gates arrive and state is
// controlled); the rows here redistribute to the top bar and the bottom
// panel as those units land.

import { useEffect, useState } from "react";
import { dismissNotification, killProcess, patchProcess } from "../api/http.ts";
import type { PendingNotification, Process, Usage } from "../api/types.ts";

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
  processes,
  usage,
  notifications,
}: {
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

  return (
    <div className="controls-strip dim">
      {error && <div className="error">{error}</div>}
      {usage && <div className="controls">{usageLine(usage)}</div>}
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
