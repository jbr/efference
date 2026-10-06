// Session-layer actions: api calls whose error recovery folds back into the
// store. Framework-free, like the rest of this layer.

import {
  ApiError,
  resolveApproval,
  submitTurn as postTurn,
  withdrawTurns,
  type WithdrawnTurn,
} from "../api/http.ts";
import type { Attachment, CallId, Decision, Gate } from "../api/types.ts";
import {
  applyFrame,
  mintTurn,
  pushPendingTurn,
  removePendingTurns,
  unmintTurn,
  type Store,
} from "./store.ts";

/**
 * Submit a turn and queue its delivery echo. The queue push happens after the
 * 202 — "accepted by the daemon, not yet on the wire" is exactly what the
 * pending state means — so a failed POST leaves nothing to reconcile and the
 * error propagates to the composer as before. The submission id is minted
 * here, before the POST: the daemon echoes it onto the resulting `User`
 * entry, so delivery is an exact key lookup — including when the entry's
 * frame folds before the 202 body resolves (`pushPendingTurn` then skips the
 * queue entirely).
 */
export async function submitTurn(
  store: Store,
  text: string,
  attachments: Attachment[] = [],
  acceptColdRead = false,
): Promise<void> {
  const id = crypto.randomUUID();
  // Recorded before the POST: an `ask` can hand this turn back while the 202
  // is still in flight (session/store.ts).
  store.update((s) => mintTurn(s, id));
  try {
    await postTurn(text, id, attachments, acceptColdRead);
  } catch (error) {
    store.update((s) => unmintTurn(s, id));
    throw error;
  }
  store.update((s) => pushPendingTurn(s, id, text, attachments));
}

/**
 * Recall a queued-but-unsent turn (docs/web-client.md): withdraw the pending-queue
 * suffix beginning at `id` and return the texts the daemon **actually** popped, in
 * submission order, for the composer to splice.
 *
 * Nothing was ever logged — a pending turn sits in the daemon's queue, unprojected —
 * so there is nothing to retract, and the echoes are dropped only for the ids that
 * came back. An empty result is the lost race (the loop dequeued it first); the caller
 * leaves its draft alone. Their attachments come back too, still stored, for the
 * composer to stage again.
 */
export async function recallTurn(
  store: Store,
  id: string,
): Promise<WithdrawnTurn[]> {
  const { withdrawn } = await withdrawTurns(id);
  store.update((s) =>
    removePendingTurns(
      s,
      withdrawn.map((t) => t.submission_id),
    ),
  );
  return withdrawn;
}

/**
 * Resolve the pending gate (docs/control-api.md). `"applied"` means the
 * decision landed; the daemon pushes the cleared/next `gate` topic on the
 * stream, so success changes nothing here. A `gate_moved` rejection means it
 * did not — that call was no longer the pending gate — and carries the
 * current gate (or null) as corrective detail; folding it as a synthetic
 * state frame re-renders the surface without a second round-trip, and the
 * `"moved"` return is how the surface knows to say so rather than treating
 * its own resolution as the cause. Anything else rethrows for the ui to
 * show.
 */
export async function resolveGate(
  store: Store,
  callId: CallId,
  decision: Decision,
): Promise<"applied" | "moved"> {
  try {
    await resolveApproval(callId, decision);
    return "applied";
  } catch (error) {
    if (error instanceof ApiError && error.code === "gate_moved") {
      const gate = (error.detail ?? null) as Gate | null;
      store.update((s) =>
        applyFrame(s, { class: "state", kind: "gate", data: gate }),
      );
      return "moved";
    }
    throw error;
  }
}
