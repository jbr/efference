// The pending approval gate: the affordance only (docs/web-client.md). The
// call it decides is already on screen — a gated call has no result entry,
// so it and its spotter verdict are the transcript's last entries, directly
// above this. Re-rendering them here duplicated the whole call body. The
// four resolutions are the decision vocabulary of docs/permissions.md.
//
// Because the affordance sits in the flow rather than pinned, it can be
// scrolled away from; `GateCue` is the fixed marker that says so, the way
// the TUI's status bar mirrors the affordance for the same case
// (docs/tui.md). It never scrolls on its own — arriving at a decision is
// the operator's move to make, not the page's.

import { useEffect, useRef, useState } from "react";
import type { Decision, Gate, Spot } from "../api/types.ts";
import { callView } from "../render/parse.ts";

/** The tool input's `command` field when it has one — the prefill for
 * allow-with-prefix. Tool-shape peeking, not a /v1 envelope field. */
const commandOf = (input: unknown): string | null =>
  typeof input === "object" &&
  input !== null &&
  "command" in input &&
  typeof (input as { command: unknown }).command === "string"
    ? (input as { command: string }).command
    : null;

export function GateSurface({
  gate,
  spot,
  resolve,
}: {
  gate: Gate;
  spot: Spot | null;
  resolve: (decision: Decision) => Promise<"applied" | "moved">;
}) {
  const [steer, setSteer] = useState("");
  const [prefix, setPrefix] = useState(() => commandOf(gate.input) ?? "");
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
  const surface = useRef<HTMLDivElement>(null);

  // The pending call changed under this surface. Always reset the inputs — a
  // decision composed against the old call must not land on the new one. Say
  // so only when this surface did not cause the change: after our own
  // resolution the next gate arriving is normal (a batch with several gated
  // calls does this on every approval, and the daemon's diffing topic
  // publisher may never show the cleared state between). An unprompted
  // change means another session decided this call.
  const [drafts, setDrafts] = useState(() => ({
    call: gate.call_id,
    prefill: commandOf(gate.input) ?? "",
  }));
  if (drafts.call !== gate.call_id) {
    const dirty = steer !== "" || note !== "" || prefix !== drafts.prefill;
    const prefill = commandOf(gate.input) ?? "";
    setDrafts({ call: gate.call_id, prefill });
    setSteer("");
    setPrefix(prefill);
    setNote("");
    setResolvedHere(false);
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

  return (
    <>
      <GateCue gate={gate} target={surface} />
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
        {notice && (
          <div className={notice.error ? "error" : "dim"}>{notice.text}</div>
        )}
        <div className="controls">
          <button
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
      </div>
    </>
  );
}

/** The fixed marker shown while the affordance is scrolled out of view. It
 * names the call — that is the part the operator cannot see when this is
 * showing.
 *
 * Activating it scrolls to the document tail rather than to the surface: a
 * gated call has no result entry, so the gate *is* the tail, and going there
 * brings the call and its verdict along. This also has to be a jump, not a
 * smooth `scrollIntoView`. `useFollowTail` pins to the bottom after every
 * render while the viewport is near it, so an animated scroll toward the tail
 * re-engages the follow partway down and is cancelled by the next render —
 * including the one this component causes when the gate comes into view. */
function GateCue({
  gate,
  target,
}: {
  gate: Gate;
  target: React.RefObject<HTMLDivElement | null>;
}) {
  const [hidden, setHidden] = useState(true);

  useEffect(() => {
    const el = target.current;
    if (el === null) return;
    const observer = new IntersectionObserver(
      ([entry]) => setHidden(entry.isIntersecting),
      { threshold: 0 },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [target]);

  if (hidden) return null;
  const { summary } = callView(gate.name, gate.input);
  return (
    <button
      className="gate-cue"
      onClick={() => window.scrollTo(0, document.documentElement.scrollHeight)}
    >
      ⏸ {gate.name}
      {summary !== null && ` · ${summary}`} — awaiting approval
    </button>
  );
}
