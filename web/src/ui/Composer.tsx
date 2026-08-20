// The composer area (docs/web-client.md): where loop state is controlled.
// Gates arrive here, so the dials that decide how the loop moves — pace,
// hold, review level, interrupt — sit on the status row above the input
// rather than in a strip at the top of the page. Submission goes through the
// session action so an accepted turn queues its delivery echo
// (docs/control-api.md — submission is decoupled from inference; a 202 is
// success, the turn's effects arrive on the stream).

import { useState } from "react";
import { interruptLoop, patchLoop } from "../api/http.ts";
import type { Pace, ReviewLevel } from "../api/types.ts";

const PACES: [number, string][] = [
  [0, "full"],
  [2000, "2s"],
  [10000, "10s"],
  [30000, "30s"],
];

const LEVELS: ReviewLevel[] = ["autonomous", "spotter_human", "human_only"];

export function Composer({
  submit: send,
  pace,
  reviewLevel,
  running,
}: {
  submit: (text: string) => Promise<void>;
  pace: Pace | null;
  reviewLevel: ReviewLevel | null;
  running: boolean;
}) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));

  async function submit() {
    const text = draft.trim();
    if (!text) return;
    try {
      await send(text);
      setDraft("");
      setError(null);
    } catch (e) {
      setError(`${e}`);
    }
  }

  const speed = pace?.speed_ms ?? 0;
  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      {error && <div className="error">{error}</div>}
      <div className="composer-status">
        <span className="turn-state">
          <span className={running ? "dot breathing" : "dot"} />
          {running ? (
            <span className="dim">running…</span>
          ) : (
            <span className="your-turn">your turn</span>
          )}
        </span>
        {/* Reserved: the key-hint strip lands here once binds exist. */}
        <span className="spacer" />
        <button
          type="button"
          disabled={!running}
          onClick={() => act(interruptLoop())}
        >
          interrupt
        </button>
        <button
          type="button"
          onClick={() => act(patchLoop({ held: !(pace?.held ?? false) }))}
        >
          {pace?.held ? "resume" : "hold"}
        </button>
        <label className="dim">
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
        <label className="dim">
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
      <div className="composer-input">
        <span className="prompt">›</span>
        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
          placeholder="send a turn (enter to send, shift-enter for newline)"
          rows={3}
        />
        {running && (
          <span className="ride-along">
            rides along with the pending result
          </span>
        )}
        <button>send</button>
      </div>
    </form>
  );
}
