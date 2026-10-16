// Standing summary action in the context popup (docs/web-client.md).
import { useState } from "react";
import { ApiError, compactLoop } from "../api/http.ts";
import type { Cold, Compaction } from "../api/types.ts";
import { compactOffer } from "./compact.ts";

export interface CompactControlProps {
  cold: Cold | null;
  compaction: Compaction | null;
  running: boolean;
  gatePending: boolean;
  asking: boolean;
}

export function CompactControl({
  cold,
  compaction,
  running,
  gatePending,
  asking,
}: CompactControlProps) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const offer = compactOffer(cold, compaction, running, gatePending, asking);
  async function start() {
    if (pending || offer.kind !== "ready") return;
    setPending(true);
    setError(null);
    try {
      await compactLoop(offer.warm);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : `${e}`);
    } finally {
      setPending(false);
    }
  }
  return (
    <div className="compact-control">
      {offer.kind === "writing" || pending ? (
        <p className="compact-progress" role="status">
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
      {compaction?.state === "failed" && (
        <div className="error">summary failed: {compaction.error}</div>
      )}
      {error && (
        <div className="error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
