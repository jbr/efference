// The composer's notice about the session's history (docs/web-client.md): shown
// while the cache is believed expired, and after a compaction failed. It states what a
// send reads uncached and offers the compaction beside it — summarize the history
// instead — and it is where a compaction's progress and failure are stated. The
// standing, confirmed compaction is the drawer's.

import type { Cold, Compaction } from "../api/types.ts";
import { type CompactOffer, kilotokens } from "./compact.ts";

export function CompactNotice({
  cold,
  compaction,
  offer,
  onCompact,
}: {
  cold: Cold | null;
  compaction: Compaction | null;
  offer: CompactOffer;
  onCompact: () => void;
}) {
  if (offer.kind === "writing") {
    return (
      <div className="compact-notice" role="status">
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
      <button type="button" className="compact-btn" onClick={onCompact}>
        {compaction?.state === "failed"
          ? "try summarizing again"
          : "summarize instead"}
      </button>
    ) : (
      <span className="dim">{offer.reason}</span>
    );
  return (
    <div className="compact-notice" role="status">
      {compaction?.state === "failed" && (
        <div className="error">summary failed: {compaction.error}</div>
      )}
      {cold !== null ? (
        <>
          cache expired — a send reads {read}, or {action}
        </>
      ) : (
        compaction?.state === "failed" && action
      )}
    </div>
  );
}
