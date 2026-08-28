// The top bar (docs/web-client.md): sticky interface chrome — the wordmark,
// the session status dot, and the usage meter. Deliberately minimal: the
// mock's bar is a dumping ground of status items, and additions happen
// one argued element at a time. Loop state is not restated here — the dot
// breathes while running, and the banner, gate cue, and composer turn-state
// carry the rest.

import type { UpstreamFailure, UpstreamRetry, Usage } from "../api/types.ts";
import type { Connection } from "../session/store.ts";
import { upstreamChip } from "./upstream.ts";

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

/** The transport chip: a retry in flight while one is, otherwise the last
 * non-success status. Both topics clear on recovery, so the chip's absence is
 * the all-clear rather than "nothing observed yet". A stalling loop with no
 * visible cause is what it is here for. Shape lives in `ui/upstream.ts`. */
function UpstreamChip({
  chip,
}: {
  chip: NonNullable<ReturnType<typeof upstreamChip>>;
}) {
  return (
    <span
      className={
        chip.retrying ? "upstream-failure retrying" : "upstream-failure"
      }
      title={chip.title}
    >
      {chip.text}
    </span>
  );
}

export function TopBar({
  running,
  connection,
  usage,
  upstreamFailure,
  upstreamRetry,
}: {
  running: boolean;
  connection: Connection;
  usage: Usage | null;
  upstreamFailure: UpstreamFailure | null;
  upstreamRetry: UpstreamRetry | null;
}) {
  const chip = upstreamChip(upstreamFailure, upstreamRetry);
  return (
    <header className="top-bar">
      <div className="top-bar-inner">
        <span className="wordmark">efference</span>
        <span
          className={running ? "dot breathing" : "dot"}
          title={`${connection}${running ? " · running" : ""}`}
        />
        <span className="spacer" />
        {chip !== null && <UpstreamChip chip={chip} />}
        {usage !== null && <UsageMeter usage={usage} />}
      </div>
    </header>
  );
}
