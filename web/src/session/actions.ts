// Session-layer actions: api calls whose error recovery folds back into the
// store. Framework-free, like the rest of this layer.

import { ApiError, resolveApproval } from "../api/http.ts";
import type { CallId, Decision, Gate } from "../api/types.ts";
import { applyFrame, type Store } from "./store.ts";

/**
 * Resolve the pending gate (docs/control-api.md). Success changes nothing
 * here — the daemon pushes the cleared/next `gate` topic on the stream. A
 * `gate_moved` rejection carries the current gate (or null) as corrective
 * detail; folding it as a synthetic state frame re-renders the surface
 * without a second round-trip. Anything else rethrows for the ui to show.
 */
export async function resolveGate(
  store: Store,
  callId: CallId,
  decision: Decision,
): Promise<void> {
  try {
    await resolveApproval(callId, decision);
  } catch (error) {
    if (error instanceof ApiError && error.code === "gate_moved") {
      const gate = (error.detail ?? null) as Gate | null;
      store.update((s) =>
        applyFrame(s, { class: "state", kind: "gate", data: gate }),
      );
      return;
    }
    throw error;
  }
}
