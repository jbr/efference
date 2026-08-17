// The pending approval gate, whole and resolvable (docs/web-client.md —
// parity of information: an informed choice means reading the entire call,
// so the input renders unabridged, in the transcript flow rather than a
// modal that hides context). The four resolutions are the decision
// vocabulary of docs/permissions.md.

import { useState } from "react";
import type { Decision, Gate, Spot } from "../api/types.ts";

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
  resolve: (decision: Decision) => Promise<void>;
}) {
  const [steer, setSteer] = useState("");
  const [prefix, setPrefix] = useState(() => commandOf(gate.input) ?? "");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  // The gate changed under this surface (resolved elsewhere, or a
  // gate_moved correction): reset the inputs and say so, because a decision
  // composed against the old call must not land on the new one.
  const [forCall, setForCall] = useState(gate.call_id);
  if (forCall !== gate.call_id) {
    setForCall(gate.call_id);
    setSteer("");
    setPrefix(commandOf(gate.input) ?? "");
    setNote("");
    setNotice("the gate moved — this is now the pending call");
  }

  async function send(decision: Decision) {
    setBusy(true);
    setNotice(null);
    try {
      await resolve(decision);
    } catch (e) {
      setNotice(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  const reason = spot ? `${spot.stance} — ${spot.reason}` : gate.reason;
  return (
    <div className="block gate">
      <div className="kind">gate: {gate.name} — awaiting approval</div>
      {reason && <div className="dim">spotter: {reason}</div>}
      <pre>{JSON.stringify(gate.input, null, 2)}</pre>
      {notice && <div className="error">{notice}</div>}
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
  );
}
