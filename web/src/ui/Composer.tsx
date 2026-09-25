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
import type { Pace, Stall } from "../api/types.ts";
import { turnState } from "./turnState.ts";
import { useDockOffset } from "./float.ts";
import { useNarrow } from "./useNarrow.ts";

export function Composer({
  submit: send,
  recalled,
  pace,
  running,
  stall,
  gatePending,
}: {
  submit: (text: string) => Promise<void>;
  /** Text a recall pulled back out of the daemon's queue, to prepend to the draft
   * (docs/web-client.md). `nonce` is what makes a repeat splice — recalling the same
   * text twice is two events, not one value. */
  recalled: { text: string; nonce: number } | null;
  pace: Pace | null;
  running: boolean;
  /** The trajectory gave up rather than finished (docs/transport.md): the
   * turn-state readout says so, since `running: false` alone cannot. */
  stall: Stall | null;
  /** A gate float may own the above-dock slot; the compose chip yields to
   * it (the float carries its own steering input). */
  gatePending: boolean;
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
  // Splice a recall in ahead of whatever is already drafted, joined with a newline —
  // adjacent pending turns are separate blocks on the wire, so popping several into one
  // buffer has to invent a join (docs/web-client.md). Keyed on the nonce so recalling
  // the same text twice splices twice; the draft is uncommitted, so nothing here touches
  // the log or the cached prefix.
  const spliced = useRef<number | null>(null);
  useEffect(() => {
    if (!recalled || spliced.current === recalled.nonce) return;
    spliced.current = recalled.nonce;
    setDraft((current) =>
      current ? `${recalled.text}\n${current}` : recalled.text,
    );
    setExpanded(true);
    field.current?.focus();
  }, [recalled]);
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
          the composer. Turn state at the left; at the right only the controls
          that apply now — interrupt while a turn runs, resume while held.
          The rest of the loop's dials are the drawer's (ui/Drawer.tsx). */}
      <div className="status-bar">
        <span
          className={
            turn.kind === "running"
              ? "turn-state dim"
              : turn.kind === "stalled"
                ? "turn-state stalled"
                : "turn-state your-turn"
          }
          title={turn.title}
        >
          {turn.text}
        </span>
        <span className="spacer" />
        {running && (
          <button type="button" onClick={() => act(interruptLoop())}>
            interrupt
          </button>
        )}
        {(pace?.held ?? false) && (
          <button type="button" onClick={() => act(patchLoop({ held: false }))}>
            resume
          </button>
        )}
      </div>
    </>
  );
}
