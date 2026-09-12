// The top bar (docs/web-client.md): sticky interface chrome — the wordmark,
// the session status dot, what the session is (model and working directory),
// and the usage meter. Deliberately minimal: the
// mock's bar is a dumping ground of status items, and additions happen
// one argued element at a time. Loop state is not restated here — the dot
// breathes while running, and the banner, gate cue, and composer turn-state
// carry the rest.

import type {
  SessionIdentity,
  UpstreamFailure,
  UpstreamRetry,
  Usage,
} from "../api/types.ts";
import type { Connection } from "../session/store.ts";
import { homeRelative } from "./identity.ts";
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

/** What this session is: its id and title (docs/session-title.md), the model
 * answering, and where it is working. Which model is running is not derivable
 * from the transcript, and it matters most when cheap and flagship models mix
 * across sessions. The cwd's shortening is `ui/identity.ts`'s, so nothing
 * about paths lives here. */
function SessionLabel({ identity }: { identity: SessionIdentity }) {
  return (
    <span className="session-id">
      <span className="session-name" title={identity.id}>
        {identity.title ?? identity.id}
      </span>
      <span
        className="session-model"
        title={
          identity.inference &&
          `spotter ${identity.inference.spotter.connection}/${identity.inference.spotter.model} · triage ${
            identity.inference.triage.shared
              ? "shared"
              : `${identity.inference.triage.connection}/${identity.inference.triage.model}`
          }`
        }
      >
        {identity.inference
          ? `${identity.inference.main.connection}/${identity.model}`
          : identity.model}
      </span>
      <span className="session-cwd dim" title={identity.cwd}>
        {homeRelative(identity.cwd)}
      </span>
    </span>
  );
}

export function TopBar({
  listHref,
  running,
  connection,
  identity,
  usage,
  upstreamFailure,
  upstreamRetry,
}: {
  /** The session list to go back to, when this page is served by a host —
   * `null` at a session daemon's root, where there is no list. */
  listHref: string | null;
  running: boolean;
  connection: Connection;
  identity: SessionIdentity | null;
  usage: Usage | null;
  upstreamFailure: UpstreamFailure | null;
  upstreamRetry: UpstreamRetry | null;
}) {
  const chip = upstreamChip(upstreamFailure, upstreamRetry);
  return (
    <header className="top-bar">
      <div className="top-bar-inner">
        {/* A plain link, because leaving a session is a navigation: the
            control base changes with it (api/base.ts). */}
        {listHref !== null && (
          <a className="to-list" href={listHref} title="all sessions">
            ↩
          </a>
        )}
        <span className="wordmark">efference</span>
        <span
          className={running ? "dot breathing" : "dot"}
          title={`${connection}${running ? " · running" : ""}`}
        />
        {identity !== null && <SessionLabel identity={identity} />}
        <span className="spacer" />
        {chip !== null && <UpstreamChip chip={chip} />}
        {usage !== null && <UsageMeter usage={usage} />}
      </div>
    </header>
  );
}
