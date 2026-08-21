// The top bar (docs/web-client.md): sticky interface chrome — the wordmark,
// the session status dot, and the usage meter. Deliberately minimal: the
// mock's bar is a dumping ground of status items, and additions happen
// one argued element at a time. Loop state is not restated here — the dot
// breathes while running, and the banner, gate cue, and composer turn-state
// carry the rest.

import type { UpstreamFailure, Usage } from "../api/types.ts";
import type { Connection } from "../session/store.ts";

const tokens = (n: number) =>
  n >= 1000 ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k` : `${n}`;

/** Context occupancy + session cost, whichever halves the provider filled;
 * the meter draws only when the context half is known. */
function UsageMeter({ usage }: { usage: Usage }) {
  const context = usage.context;
  const used = context?.used ?? null;
  return (
    <>
      {context !== null && (
        <>
          <span className="dim">
            ctx{" "}
            <span className="usage-strong">
              {used === null ? "?" : tokens(used)}
            </span>
            /{tokens(context.limit)}
          </span>
          <span className="meter">
            <span
              className="meter-fill"
              style={{
                width:
                  used === null
                    ? 0
                    : `${Math.min(100, (used / context.limit) * 100)}%`,
              }}
            />
          </span>
        </>
      )}
      {usage.cost !== null && (
        <span className="dim">
          ${usage.cost.usd.toFixed(4)}
          {usage.cost.unpriced > 0 ? ` (+${usage.cost.unpriced} unpriced)` : ""}
        </span>
      )}
    </>
  );
}

/** The last non-success upstream response. It stays after a success — the
 * daemon defines no all-clear boundary — so it reads as "last observed",
 * not "currently failing"; the title says so. A stalling loop with no
 * visible cause is what it is here for. */
function UpstreamFailureChip({ failure }: { failure: UpstreamFailure }) {
  return (
    <span
      className="upstream-failure"
      title="last non-success upstream response (not cleared by a later success)"
    >
      upstream {failure.status}
      {failure.count > 1 && ` ×${failure.count}`}
    </span>
  );
}

export function TopBar({
  running,
  connection,
  usage,
  upstreamFailure,
}: {
  running: boolean;
  connection: Connection;
  usage: Usage | null;
  upstreamFailure: UpstreamFailure | null;
}) {
  return (
    <header className="top-bar">
      <div className="top-bar-inner">
        <span className="wordmark">efference</span>
        <span
          className={running ? "dot breathing" : "dot"}
          title={`${connection}${running ? " · running" : ""}`}
        />
        <span className="spacer" />
        {upstreamFailure !== null && (
          <UpstreamFailureChip failure={upstreamFailure} />
        )}
        {usage !== null && <UsageMeter usage={usage} />}
      </div>
    </header>
  );
}
