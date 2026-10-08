// The drawer's standing fold (docs/web-client.md): summarize the session's
// history on request, warm or cold. The composer's cold notice is the answer to
// a refusal; this is the always-there version, behind a confirm because a fold
// has no undo yet. States the same three things the notice does — offered,
// writing, blocked — from `foldOffer`.

import { useState } from "react";
import { ApiError, foldLoop } from "../api/http.ts";
import type { Cold, Fold } from "../api/types.ts";
import { foldOffer, foldWarning } from "./fold.ts";

export function FoldControl({
  cold,
  fold,
  running,
  gatePending,
  asking,
}: {
  cold: Cold | null;
  fold: Fold | null;
  running: boolean;
  gatePending: boolean;
  asking: boolean;
}) {
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const offer = foldOffer(cold, fold, running, gatePending, asking);

  function start() {
    setConfirming(false);
    foldLoop(offer.warm)
      .then(() => setError(null))
      .catch((e) => setError(e instanceof ApiError ? e.message : `${e}`));
  }

  return (
    <div className="fold-control">
      <p className="fold-note">
        Replace the conversation so far with a summary the session's model
        writes, keeping the most recent work verbatim.
      </p>
      {offer.kind === "writing" ? (
        <p className="fold-progress" role="status">
          Summarizing the history…
        </p>
      ) : confirming ? (
        <div className="fold-confirm">
          <p className="fold-warning">{foldWarning(cold)}</p>
          <button type="button" className="danger" onClick={start}>
            Summarize
          </button>
          <button type="button" onClick={() => setConfirming(false)}>
            Cancel
          </button>
        </div>
      ) : (
        <button
          type="button"
          disabled={offer.kind === "blocked"}
          title={offer.reason}
          onClick={() => setConfirming(true)}
        >
          Summarize history…
        </button>
      )}
      {offer.kind === "blocked" && <p className="dim">{offer.reason}</p>}
      {fold?.state === "failed" && (
        <div className="error">summary failed: {fold.error}</div>
      )}
      {error && <div className="error">{error}</div>}
    </div>
  );
}
