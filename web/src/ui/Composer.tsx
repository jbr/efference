// The composer area (docs/web-client.md): where loop state is controlled.
// Gates arrive here, so the dials that decide how the loop moves — pace,
// hold, review level, interrupt — sit on the status row above the input
// rather than in a strip at the top of the page. Submission goes through the
// session action so an accepted turn queues its delivery echo
// (docs/control-api.md — submission is decoupled from inference; a 202 is
// success, the turn's effects arrive on the stream).

import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  continueLoop,
  interruptLoop,
  patchLoop,
} from "../api/http.ts";
import type { Pace, ReviewLevel, Stall } from "../api/types.ts";
import { turnState } from "./turnState.ts";
import { useDockOffset } from "./float.ts";
import { useNarrow } from "./useNarrow.ts";

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
  stall,
  gatePending,
  quiet,
  onReveal,
}: {
  submit: (text: string) => Promise<void>;
  pace: Pace | null;
  reviewLevel: ReviewLevel | null;
  running: boolean;
  /** The trajectory gave up rather than finished (docs/transport.md): the
   * turn-state readout says so, since `running: false` alone cannot. */
  stall: Stall | null;
  /** A gate float may own the above-dock slot; the compose chip yields to
   * it (the float carries its own steering input). */
  gatePending: boolean;
  /** Narrow chrome quieting: dials hidden until the ⋯ reveal (App state,
   * shared with the panel strip). */
  quiet: boolean;
  onReveal: () => void;
}) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const narrow = useNarrow();
  // On narrow the composer band collapses when not in use — the dock is a
  // third of a phone screen otherwise — and the floating compose chip
  // summons it (docs/web-client.md "The phone breakpoint"). The draft
  // survives collapse; only the band's visibility changes.
  const [expanded, setExpanded] = useState(false);
  const field = useRef<HTMLTextAreaElement>(null);
  const fabBottom = useDockOffset();
  const collapsed = narrow && !expanded;
  useEffect(() => {
    if (narrow && expanded) field.current?.focus();
  }, [narrow, expanded]);
  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));

  async function submit() {
    const text = draft.trim();
    // An empty send hands control back with no input (docs/web-client.md): the
    // loop takes another turn on the wire it already has. A decline is a normal
    // answer — the moment had no continue in it — so its message renders on its
    // own rather than as a thrown-error string.
    if (!text) {
      try {
        await continueLoop();
        setError(null);
      } catch (e) {
        setError(e instanceof ApiError ? e.message : `${e}`);
      }
      return;
    }
    try {
      await send(text);
      setDraft("");
      setError(null);
      setExpanded(false);
    } catch (e) {
      setError(`${e}`);
    }
  }

  const speed = pace?.speed_ms ?? 0;
  const turn = turnState(running, stall);
  return (
    <>
      {collapsed && !gatePending && (
        <button
          className="compose-fab"
          style={{ bottom: fabBottom }}
          onClick={() => setExpanded(true)}
        >
          ✎ {draft.trim() ? "draft…" : "compose"}
        </button>
      )}
      <form
        className="composer"
        hidden={collapsed}
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <div className="col composer-inner">
          {error && <div className="error">{error}</div>}
          <div className="composer-input">
            <span className="prompt">›</span>
            <textarea
              ref={field}
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                // On a phone there is no shift-enter, so intercepting Enter
                // would make multi-line drafts impossible: return inserts a
                // newline and the send button sends.
                if (!narrow && event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  void submit();
                }
                if (narrow && event.key === "Escape") setExpanded(false);
              }}
              placeholder={
                narrow
                  ? "send a turn"
                  : "send a turn (enter to send, shift-enter for newline)"
              }
              rows={3}
            />
            {narrow && (
              <button
                type="button"
                className="linkish"
                title="put the composer away; the draft keeps"
                onClick={() => setExpanded(false)}
              >
                ⌄
              </button>
            )}
            <button>send</button>
          </div>
        </div>
      </form>
      {/* The status bar: the dock's outermost row, at the window edge below
          the composer — chrome frames content, as in a native status bar.
          Full-bleed: turn state at the far left, the loop dials at the far
          right. */}
      <div className="status-bar">
        <span className="turn-state">
          <span className={running ? "dot breathing" : "dot"} />
          <span
            className={
              turn.kind === "running"
                ? "dim"
                : turn.kind === "stalled"
                  ? "stalled"
                  : "your-turn"
            }
            title={turn.title}
          >
            {turn.text}
          </span>
        </span>
        {/* Reserved: the key-hint strip lands here once binds exist. */}
        <span className="spacer" />
        {/* Quiet chrome: on narrow the dials hide until ⋯ reveals them —
            interrupt stays whenever it is usable (it is the control a phone
            wants when a run goes wrong), held pace stays because a hidden
            hold reads as a hang. */}
        {(!quiet || running) && (
          <button
            type="button"
            disabled={!running}
            onClick={() => act(interruptLoop())}
          >
            interrupt
          </button>
        )}
        {(!quiet || (pace?.held ?? false)) && (
          <button
            type="button"
            onClick={() => act(patchLoop({ held: !(pace?.held ?? false) }))}
          >
            {pace?.held ? "resume" : "hold"}
          </button>
        )}
        {!quiet && (
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
        )}
        {!quiet && (
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
        )}
        {narrow && (
          <button
            type="button"
            className="linkish"
            title={quiet ? "show the dials and panel tabs" : "hide them again"}
            onClick={onReveal}
          >
            {quiet ? "⋯" : "×"}
          </button>
        )}
      </div>
    </>
  );
}
