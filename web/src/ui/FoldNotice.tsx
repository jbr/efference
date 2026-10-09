// The composer's notice about the session's history (docs/web-client.md): shown
// while the cache is believed expired, and after a fold failed. It states what a
// send reads uncached and offers the fold beside it — summarize the history
// instead — and it is where a fold's progress and failure are stated. The
// standing, confirmed fold is the drawer's.

import type { Cold, Fold } from "../api/types.ts";
import { type FoldOffer, kilotokens } from "./fold.ts";

export function FoldNotice({
  cold,
  fold,
  offer,
  onFold,
}: {
  cold: Cold | null;
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
      ? `~${kilotokens(tokens)} tokens uncached`
      : "the context uncached";
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
      {cold !== null ? (
        <>
          cache expired — a send reads {read}, or {action}
        </>
      ) : (
        fold?.state === "failed" && action
      )}
    </div>
  );
}
