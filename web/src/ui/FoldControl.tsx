// Standing summary action in the context popup (docs/web-client.md).
import { useState } from "react";
import { ApiError, foldLoop } from "../api/http.ts";
import type { Cold, Fold } from "../api/types.ts";
import { foldOffer } from "./fold.ts";

export interface FoldControlProps {
  cold: Cold | null;
  fold: Fold | null;
  running: boolean;
  gatePending: boolean;
  asking: boolean;
}

export function FoldControl({
  cold,
  fold,
  running,
  gatePending,
  asking,
}: FoldControlProps) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const offer = foldOffer(cold, fold, running, gatePending, asking);
  async function start() {
    if (pending || offer.kind !== "ready") return;
    setPending(true);
    setError(null);
    try {
      await foldLoop(offer.warm);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : `${e}`);
    } finally {
      setPending(false);
    }
  }
  return (
    <div className="fold-control">
      {offer.kind === "writing" || pending ? (
        <p className="fold-progress" role="status">
          Summarizing the history…
        </p>
      ) : (
        <button
          type="button"
          disabled={offer.kind === "blocked"}
          title={offer.reason}
          onClick={() => void start()}
        >
          Summarize history
        </button>
      )}
      {offer.kind === "blocked" && <p className="dim">{offer.reason}</p>}
      {fold?.state === "failed" && (
        <div className="error">summary failed: {fold.error}</div>
      )}
      {error && (
        <div className="error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
