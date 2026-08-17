// The operator dials (docs/web-client.md), from the topics the store already
// carries: interrupt, pace + hold, review level, and the per-process verbs
// (background handoff on the foreground entry, kill). Rendered under the
// header — control furniture, not transcript content.

import { useState } from "react";
import {
  interruptLoop,
  killProcess,
  patchLoop,
  patchProcess,
} from "../api/http.ts";
import type { Pace, Process, ReviewLevel } from "../api/types.ts";

const PACES: [number, string][] = [
  [0, "full"],
  [2000, "2s"],
  [10000, "10s"],
  [30000, "30s"],
];

const LEVELS: ReviewLevel[] = ["autonomous", "spotter_human", "human_only"];

export function Controls({
  pace,
  reviewLevel,
  running,
  processes,
}: {
  pace: Pace | null;
  reviewLevel: ReviewLevel | null;
  running: boolean;
  processes: Process[];
}) {
  const [error, setError] = useState<string | null>(null);
  // One note draft suffices: only the single foreground process offers it.
  const [note, setNote] = useState("");
  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));

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
      </div>
      {processes.map((proc) => (
        <div className="controls" key={proc.id}>
          <span>
            {proc.background ? "bg" : "fg"} {proc.id}: {proc.command}
          </span>
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
    </div>
  );
}
