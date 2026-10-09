// The composer's notice about the session's history (docs/web-client.md): shown
// after a `409 cold` refusal, and after a fold failed. It is where the second
// answer to a cold session sits beside the first — send again to pay the
// uncached read, or fold the history into a summary instead — and where a fold's
// progress and failure are stated. The standing, confirmed fold is the drawer's.

import type { Cold, Fold } from "../api/types.ts";
import { type FoldOffer, kilotokens } from "./fold.ts";

export function FoldNotice({
  cold,
  consent,
  parked,
  fold,
  offer,
  onFold,
}: {
  cold: Cold | null;
  /** A cold refusal is waiting for the operator's answer. */
  consent: boolean;
  /** The loop is stopped at its cold check (the `park` topic): the same choice, offered
   * without a refusal first, so it is "send", not "send again". */
  parked: boolean;
  fold: Fold | null;
  offer: FoldOffer;
  onFold: () => void;
}) {
  if (offer.kind === "writing") {
    return (
      <div className="fold-notice" role="status">
        summarizing the history — the session's model is writing the account…
      </div>
    );
  }
  const tokens = cold?.context_tokens ?? null;
  const read =
    tokens !== null
      ? `read ${kilotokens(tokens)} tokens uncached`
      : "read it uncached";
  const action =
    offer.kind === "ready" ? (
      <button type="button" className="fold-btn" onClick={onFold}>
        {fold?.state === "failed"
          ? "try summarizing again"
          : "summarize instead"}
      </button>
    ) : (
      <span className="dim">{offer.reason}</span>
    );
  return (
    <div className="fold-notice" role="status">
      {fold?.state === "failed" && (
        <div className="error">summary failed: {fold.error}</div>
      )}
      {(consent || parked) && cold !== null ? (
        <>
          cache believed cold — send{consent ? " again" : ""} to {read}, or{" "}
          {action}
        </>
      ) : (
        fold?.state === "failed" && action
      )}
    </div>
  );
}
