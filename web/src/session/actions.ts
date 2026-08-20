// Session-layer actions: api calls whose error recovery folds back into the
// store. Framework-free, like the rest of this layer.

import {
  ApiError,
  resolveApproval,
  submitTurn as postTurn,
} from "../api/http.ts";
import type { CallId, Decision, Gate } from "../api/types.ts";
import { applyFrame, pushPendingTurn, type Store } from "./store.ts";

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
export async function submitTurn(store: Store, text: string): Promise<void> {
  const id = crypto.randomUUID();
  await postTurn(text, id);
  store.update((s) => pushPendingTurn(s, id, text));
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
