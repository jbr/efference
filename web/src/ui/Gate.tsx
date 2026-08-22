// The pending approval gate (docs/web-client.md). In the flow, the surface
// is the affordance only — a gated call has no result entry, so it and its
// spotter verdict are the transcript's last entries, directly above, and
// re-rendering them here duplicated the whole call body. The floating panel
// is the deliberate exception: it shows exactly while those entries are
// scrolled out of view, so it carries the call (intent, full command) and
// the verdict alongside the controls. The four resolutions are the decision
// vocabulary of docs/permissions.md.

import { useEffect, useRef, useState } from "react";
import type { Decision, Gate, Spot } from "../api/types.ts";
import { callView } from "../render/parse.ts";
import { jumpToTail, useDockOffset } from "./float.ts";
import { CallBlock } from "./ToolBlocks.tsx";
import { SpotLine } from "./Transcript.tsx";

/** The tool input's `command` field when it has one. Tool-shape peeking, not
 * a /v1 envelope field. */
const commandOf = (input: unknown): string | null =>
  typeof input === "object" &&
  input !== null &&
  "command" in input &&
  typeof (input as { command: unknown }).command === "string"
    ? (input as { command: string }).command
    : null;

/** The allow-with-prefix prefill: the prefix the spotter's promotion probe
 * named (docs/permissions.md), so the operator ratifies the same candidate
 * the spotter answered for. Falls back to the call's whole command — a
 * full-command prefix allows exactly this command — when no probe rode the
 * call. */
const prefillFor = (gate: Gate, spot: Spot | null): string =>
  spot?.probed_prefix ?? commandOf(gate.input) ?? "";

export function GateSurface({
  gate,
  spot,
  covered,
  resolve,
}: {
  gate: Gate;
  spot: Spot | null;
  /** The file viewer is open over the transcript: the in-flow surface may be
   * geometrically in view yet occluded, which the IntersectionObserver
   * cannot see — so treat it as away and float. */
  covered: boolean;
  resolve: (decision: Decision) => Promise<"applied" | "moved">;
}) {
  const [steer, setSteer] = useState("");
  const [prefix, setPrefix] = useState(() => prefillFor(gate, spot));
  const [note, setNote] = useState("");
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
  const surface = useRef<HTMLDivElement>(null);
  const bottom = useDockOffset();

  // The pending call changed under this surface. Always reset the inputs — a
  // decision composed against the old call must not land on the new one. Say
  // so only when this surface did not cause the change: after our own
  // resolution the next gate arriving is normal (a batch with several gated
  // calls does this on every approval, and the daemon's diffing topic
  // publisher may never show the cleared state between). An unprompted
  // change means another session decided this call.
  const [drafts, setDrafts] = useState(() => ({
    call: gate.call_id,
    prefill: prefillFor(gate, spot),
  }));
  if (drafts.call !== gate.call_id) {
    const dirty = steer !== "" || note !== "" || prefix !== drafts.prefill;
    const prefill = prefillFor(gate, spot);
    setDrafts({ call: gate.call_id, prefill });
    setSteer("");
    setPrefix(prefill);
    setNote("");
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
    const el = surface.current;
    if (el === null) return;
    const observer = new IntersectionObserver(
      ([entry]) => setAway(!entry.isIntersecting),
      { threshold: 0 },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

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
      <div className="controls">
        <button
          className="primary"
          disabled={busy}
          onClick={() => void send({ decision: "approve" })}
        >
          approve
        </button>
        <button
          disabled={busy}
          onClick={() => void send({ decision: "abort" })}
        >
          abort
        </button>
      </div>
      <div className="controls">
        <input
          value={steer}
          onChange={(e) => setSteer(e.target.value)}
          placeholder="steering text — what to do instead"
        />
        <button
          disabled={busy || !steer.trim()}
          onClick={() =>
            void send({ decision: "redirect", message: steer.trim() })
          }
        >
          redirect
        </button>
      </div>
      <div className="controls">
        <input
          value={prefix}
          onChange={(e) => setPrefix(e.target.value)}
          placeholder="command prefix to allow from now on"
        />
        <input
          value={note}
          onChange={(e) => setNote(e.target.value)}
          placeholder="note (optional)"
        />
        <button
          disabled={busy || !prefix.trim()}
          onClick={() =>
            void send({
              decision: "config_edit",
              change: { command: { command: prefix.trim() } },
              note: note.trim() || undefined,
            })
          }
        >
          allow prefix
        </button>
      </div>
    </>
  );

  const { summary } = callView(gate.name, gate.input);
  return (
    <>
      {(away || covered) &&
        (floatOpen ? (
          <div className="gate-float" style={{ bottom }}>
            <div className="gate-float-head">
              <span className="kind">⏸ awaiting approval</span>
              <span className="spacer" />
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
            <div className="gate-float-body">
              <CallBlock
                call={{ id: gate.call_id, name: gate.name, input: gate.input }}
              />
              {spot !== null ? (
                <SpotLine spot={spot} />
              ) : (
                gate.reason !== undefined && (
                  <div className="dim">spotter: {gate.reason}</div>
                )
              )}
            </div>
            {controls()}
          </div>
        ) : (
          <button
            className="gate-cue"
            style={{ bottom }}
            onClick={() => setFloatOpen(true)}
          >
            ⏸ {gate.name}
            {summary !== null && ` · ${summary}`} — awaiting approval
          </button>
        ))}
      <div className="block gate" ref={surface}>
        <div className="kind">⏸ awaiting approval</div>
        {/* The spotter's verdict is its own `Spot` entry directly above, so
            it is not restated here. This fallback covers a gate held with no
            `Spot` entry behind it — a window the stream's coherence rule
            closes (docs/control-api.md), so it is robustness, not a path
            that normally renders. */}
        {!spot && gate.reason && (
          <div className="dim">spotter: {gate.reason}</div>
        )}
        {controls()}
      </div>
    </>
  );
}
