// The pending approval gate (docs/web-client.md). In the flow, the decision
// renders inside the gated call's own entry (Transcript.tsx's gate slot),
// under the spotter's reason — the call it decides is the thing on screen,
// not a panel after it. The floating panel is that entry lifted: it shows
// exactly while the entry is scrolled out of view, so it carries the call
// (intent, full command) and the reason alongside the controls. The
// resolutions offered are approve, abort, and redirect from the decision
// vocabulary of docs/permissions.md; a grant (`config_edit`) waits on the
// host proposing one (docs/web-client.md).

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Cold, Decision, Gate, Spot } from "../api/types.ts";
import { coldLine as coldLineFor } from "./coldLine.ts";
import { callView } from "./signature.ts";
import { flatSignature } from "./signatureLayout.ts";
import { jumpToTail, useDockOffset } from "./float.ts";
import { CallBlock } from "./ToolBlocks.tsx";
import { SpotLine } from "./Transcript.tsx";
import "./Gate.css";

/** The redirect's text: a textarea that grows with what is typed, so the
 * message is never cut off. Enter sends; shift-enter is a newline. */
function SteerField({
  value,
  onChange,
  onSubmit,
}: {
  value: string;
  onChange: (text: string) => void;
  onSubmit: () => void;
}) {
  const field = useRef<HTMLTextAreaElement>(null);
  useEffect(() => field.current?.focus(), []);
  return (
    <textarea
      ref={field}
      className="gate-field"
      rows={1}
      value={value}
      placeholder="Tell the agent what to do instead"
      onChange={(event) => onChange(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter" && !event.shiftKey) {
          event.preventDefault();
          onSubmit();
        }
      }}
    />
  );
}

/** The decision: allow, or decline — which opens the redirect (decline and
 * say what to do instead) and, less prominently, stopping the turn. */
function GateControls({
  busy,
  coldLine,
  steer,
  setSteer,
  approve,
  abort,
  redirect,
}: {
  busy: boolean;
  /** The cold line (ui/coldLine.ts), while the session is believed cold. */
  coldLine: string | null;
  steer: string;
  setSteer: (text: string) => void;
  approve: () => void;
  abort: () => void;
  redirect: () => void;
}) {
  const [declining, setDeclining] = useState(false);
  return (
    <div className="gate-controls">
      {coldLine !== null && (
        <div className="cold-line" role="status">
          {coldLine}
        </div>
      )}
      <div className="gate-actions">
        <button className="primary" disabled={busy} onClick={approve}>
          Allow
        </button>
        <button
          disabled={busy}
          aria-expanded={declining}
          onClick={() => setDeclining(!declining)}
        >
          Decline…
        </button>
      </div>
      {declining && (
        <div className="gate-decline">
          <div className="gate-row">
            <SteerField
              value={steer}
              onChange={setSteer}
              onSubmit={() => steer.trim() && redirect()}
            />
            <button
              className="primary"
              disabled={busy || !steer.trim()}
              onClick={redirect}
            >
              Send
            </button>
          </div>
          <button className="gate-link" disabled={busy} onClick={abort}>
            Or stop this turn entirely
          </button>
        </div>
      )}
    </div>
  );
}

/** The gated entry's horizontal box, so the floating panel sits exactly
 * above where the entry is in the flow — the entry lifted, not a separate
 * panel. Null until the entry is mounted; the float then centers (CSS). */
function useEntryColumn(
  slot: HTMLDivElement | null,
): { left: number; width: number; transform: "none" } | null {
  const [box, setBox] = useState<{ left: number; width: number } | null>(null);
  useEffect(() => {
    const entry = slot?.closest(".t-entry");
    if (!(entry instanceof HTMLElement)) return;
    const update = () => {
      const rect = entry.getBoundingClientRect();
      setBox({ left: rect.left, width: rect.width });
    };
    update();
    // The entry's width follows the column; its left edge also moves when
    // the viewport does, which a ResizeObserver on the entry alone misses.
    const observer = new ResizeObserver(update);
    observer.observe(entry);
    window.addEventListener("resize", update);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
    };
  }, [slot]);
  return slot === null || box === null ? null : { ...box, transform: "none" };
}

export function GateSurface({
  gate,
  spot,
  covered,
  slot,
  cold,
  resolve,
}: {
  gate: Gate;
  spot: Spot | null;
  /** The file viewer is open over the transcript: the in-flow surface may be
   * geometrically in view yet occluded, which the IntersectionObserver
   * cannot see — so treat it as away and float. */
  covered: boolean;
  /** The gated call's own slot in the transcript (Transcript.tsx), when it is
   * mounted: the decision renders there, inside the call it is about. */
  slot: HTMLDivElement | null;
  /** The `cold` topic: while set, the controls carry the cold line
   * (ui/coldLine.ts). */
  cold: Cold | null;
  resolve: (decision: Decision) => Promise<"applied" | "moved">;
}) {
  const [steer, setSteer] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{
    text: string;
    error: boolean;
  } | null>(null);
  // Whether this surface's own decision resolved the call it was showing.
  // The distinction the pending-call change below turns on: after our own
  // approve, the next call gating is the expected course of events, not news.
  const [resolvedHere, setResolvedHere] = useState(false);
  // The floating panel collapses to the compact cue without resolving the
  // gate — it may be covering text the operator is reading. Expanded again
  // per call.
  const [floatOpen, setFloatOpen] = useState(true);
  // The in-flow surface is scrolled out of view; the floating panel or cue
  // stands in for it. It never scrolls on its own — arriving at a decision
  // is the operator's move to make, not the page's.
  const [away, setAway] = useState(false);
  // State rather than a ref: the in-flow surface moves between the column
  // and the call's slot, and the observer has to follow it.
  const [surface, setSurface] = useState<HTMLDivElement | null>(null);
  const bottom = useDockOffset();
  const lift = useEntryColumn(slot);

  // The pending call changed under this surface. Always reset the inputs — a
  // decision composed against the old call must not land on the new one. Say
  // so only when this surface did not cause the change: after our own
  // resolution the next gate arriving is normal (a batch with several gated
  // calls does this on every approval, and the daemon's diffing topic
  // publisher may never show the cleared state between). An unprompted
  // change means another session decided this call.
  const [draftCall, setDraftCall] = useState(gate.call_id);
  if (draftCall !== gate.call_id) {
    const dirty = steer !== "";
    setDraftCall(gate.call_id);
    setSteer("");
    setResolvedHere(false);
    setFloatOpen(true);
    setNotice(
      resolvedHere
        ? null
        : {
            text: dirty
              ? "that call was resolved in another session — this is a different call, and the draft composed against the old one was cleared"
              : "that call was resolved in another session — this is a different call",
            error: false,
          },
    );
  }

  useEffect(() => {
    if (surface === null) return;
    const observer = new IntersectionObserver(
      ([entry]) => setAway(!entry.isIntersecting),
      { threshold: 0 },
    );
    observer.observe(surface);
    return () => observer.disconnect();
  }, [surface]);

  const coldLine = coldLineFor(cold, "approving or redirecting");

  async function send(decision: Decision) {
    setBusy(true);
    setNotice(null);
    try {
      if ((await resolve(decision)) === "applied") {
        setResolvedHere(true);
      } else {
        setNotice({
          text: "that decision did not land — the call had already been resolved elsewhere",
          error: false,
        });
      }
    } catch (e) {
      setNotice({ text: `${e}`, error: true });
    } finally {
      setBusy(false);
    }
  }

  // One set of form state, rendered in both places (only one is on screen at
  // a time): a draft typed in the flow survives into the floating panel and
  // back.
  const controls = () => (
    <>
      {notice && (
        <div className={notice.error ? "error" : "dim"}>{notice.text}</div>
      )}
      <GateControls
        busy={busy}
        coldLine={coldLine}
        steer={steer}
        setSteer={setSteer}
        approve={() => void send({ decision: "approve" })}
        abort={() => void send({ decision: "abort" })}
        redirect={() =>
          void send({ decision: "redirect", message: steer.trim() })
        }
      />
    </>
  );
  // The spotter's reason, as the gated call shows it — or, for a gate held
  // with no `Spot` entry behind it (a window the stream's coherence rule
  // closes, docs/control-api.md), the gate's own.
  const reason =
    spot !== null ? (
      <SpotLine spot={spot} />
    ) : (
      gate.reason !== undefined && <p className="spot">{gate.reason}</p>
    );

  const signature = flatSignature(callView(gate.name, gate.input));
  return (
    <>
      {(away || covered) &&
        (floatOpen ? (
          <div className="gate-float" style={{ bottom, ...lift }}>
            <div className="gate-float-head">
              {/* Jumping to the tail while the viewer covers it would land
                  behind the viewer. */}
              {!covered && (
                <button className="linkish" onClick={jumpToTail}>
                  jump to call ↓
                </button>
              )}
              <button className="linkish" onClick={() => setFloatOpen(false)}>
                collapse ✕
              </button>
            </div>
            {/* The transcript's own call markup and classes, so the lifted
                entry reads exactly as the one in the flow. */}
            <div className="gate-float-body transcript">
              <div className="call">
                <CallBlock
                  call={{
                    id: gate.call_id,
                    name: gate.name,
                    input: gate.input,
                  }}
                />
                {reason}
              </div>
            </div>
            {controls()}
          </div>
        ) : (
          <button
            className="gate-cue"
            style={{ bottom }}
            onClick={() => setFloatOpen(true)}
          >
            ⏸ {signature} — awaiting approval
          </button>
        ))}
      {slot !== null ? (
        createPortal(
          <div ref={setSurface}>
            {spot === null && reason}
            {controls()}
          </div>,
          slot,
        )
      ) : (
        // The gated call is not in the transcript yet — the same momentary
        // window as the missing `Spot` above.
        <div className="gate-inflow" ref={setSurface}>
          {reason}
          {controls()}
        </div>
      )}
    </>
  );
}
