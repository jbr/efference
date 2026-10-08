// The top bar (docs/web-client.md): sticky interface chrome — the wordmark
// (the way back to the session list), the session's title, the context meter,
// and the control that opens the drawer. Deliberately minimal: what the
// session is (models, directory, cost) and how it runs are the drawer's, and
// additions here happen one argued element at a time. Loop state is not
// restated here: the banner, gate cue, and composer turn-state carry it.

import type {
  SessionIdentity,
  UpstreamFailure,
  UpstreamRetry,
  Usage,
} from "../api/types.ts";
import { sessionId } from "../api/base.ts";
import { listedTitle } from "../session/listing.ts";
import { upstreamChip } from "./upstream.ts";
import { AppHeader } from "./AppHeader.tsx";
import { SessionSwitcher } from "./SessionSwitcher.tsx";

const tokens = (n: number) =>
  n >= 1_000_000
    ? `${+(n / 1_000_000).toFixed(1)}M`
    : n >= 1000
      ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k`
      : `${n}`;

/** Context occupancy: the bar is the reading, the numbers a caption under it
 * so they take no width of their own. Drawn only when the context is known; an
 * endpoint that reported no window gets the count alone, with no bar to fill. */
function ContextMeter({ context }: { context: NonNullable<Usage["context"]> }) {
  const { used, limit } = context;
  const usedText = used === null ? "?" : used.toLocaleString();
  return (
    <span
      className="context-meter"
      title={
        limit === null
          ? `context: ${usedText} tokens (window unknown)`
          : `context: ${usedText} of ${limit.toLocaleString()} tokens`
      }
    >
      {limit !== null && (
        <span className="meter">
          <span
            className="meter-fill"
            style={{
              width:
                used === null ? 0 : `${Math.min(100, (used / limit) * 100)}%`,
            }}
          />
        </span>
      )}
      <span className="context-caption">
        {used === null ? "–" : tokens(used)}
        {limit !== null && `/${tokens(limit)}`}
      </span>
    </span>
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
  listHref,
  identity,
  usage,
  upstreamFailure,
  upstreamRetry,
  drawerOpen,
  attention,
  onDrawer,
}: {
  /** The session list to go back to, when this page is served by a host —
   * `null` at a session daemon's root, where there is no list. */
  listHref: string | null;
  identity: SessionIdentity | null;
  usage: Usage | null;
  upstreamFailure: UpstreamFailure | null;
  upstreamRetry: UpstreamRetry | null;
  drawerOpen: boolean;
  /** Things in the drawer asking for attention (`drawerAttention`). */
  attention: number;
  onDrawer: () => void;
}) {
  const chip = upstreamChip(upstreamFailure, upstreamRetry);
  const routeId = sessionId();
  const context = usage?.context ?? null;
  return (
    <AppHeader
      homeHref={listHref}
      below={
        // Only under a host are there other sessions to switch to.
        // The page's own session, not the identity topic's: that is absent
        // from a session's first paint, and the row should not wait for it.
        listHref !== null && routeId !== null ? (
          <SessionSwitcher currentId={routeId} />
        ) : undefined
      }
    >
      {(identity !== null || routeId !== null) && (
        <span className="session-name" title={identity?.id ?? routeId ?? ""}>
          {identity?.title ??
            (routeId !== null ? listedTitle(routeId) : null) ??
            identity?.id ??
            routeId}
        </span>
      )}
      <span className="spacer" />
      {chip !== null && <UpstreamChip chip={chip} />}
      {context !== null && <ContextMeter context={context} />}
      <button
        className={drawerOpen ? "drawer-toggle open" : "drawer-toggle"}
        aria-expanded={drawerOpen}
        onClick={onDrawer}
      >
        Session
        {attention > 0 && <span className="drawer-count">{attention}</span>}
      </button>
    </AppHeader>
  );
}
