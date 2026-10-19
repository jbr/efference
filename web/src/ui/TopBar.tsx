import { ContextPopup } from "./ContextPopup.tsx";
import type { CompactControlProps } from "./CompactControl.tsx";
import { AppSettingsLink } from "./AppSettings.tsx";
// The top bar (docs/web-client.md): sticky interface chrome — the wordmark
// (the way back to the session list), the session's title, the context meter,
// the session cost where the provider prices requests, and the app-settings
// link. Deliberately minimal: additions here happen one argued element at a
// time. Loop state is not restated here: the banner, gate cue, and composer
// turn-state carry it.

import type {
  SessionIdentity,
  UpstreamFailure,
  UpstreamRetry,
  Usage,
} from "../api/types.ts";
import { sessionId } from "../api/base.ts";
import { sessionName } from "./identity.ts";
import { upstreamChip } from "./upstream.ts";
import { ContextBar } from "./ContextBar.tsx";
import { PlanMeter } from "./PlanMeter.tsx";
import { costLabel } from "./cost.ts";
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
export function ContextMeter({
  context,
}: {
  context: NonNullable<Usage["context"]>;
}) {
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
      {limit !== null && <ContextBar used={used} limit={limit} />}
      <span className="context-caption">
        {used === null ? "–" : tokens(used)}
        {limit !== null && `/${tokens(limit)}`}
      </span>
    </span>
  );
}

/** Session cost: the provider's billed running total, as a caption beside the
 * context meter. Absent on a provider that prices no requests — a direct
 * Anthropic session has none. `costLabel` marks the total `~` once any request
 * went unpriced, so it reads as a floor; how many did is in the title, since
 * past the first the count adds nothing. */
export function CostMeter({ cost }: { cost: NonNullable<Usage["cost"]> }) {
  const label = costLabel(cost);
  return (
    <span
      className="cost-meter"
      title={
        cost.unpriced > 0
          ? `cost: ${label} · ${cost.unpriced} unpriced`
          : `cost: ${label}`
      }
    >
      <span className="context-meter">
        {/* The meter's bar, kept only for the line it gives the caption. */}
        <span className="meter cost-meter-gap" aria-hidden="true" />
        <span className="context-caption">{label}</span>
      </span>
    </span>
  );
}

/** Latest upstream explanation and separate retry progress (docs/web-client.md). */
export function UpstreamNotice({
  chip,
}: {
  chip: NonNullable<ReturnType<typeof upstreamChip>>;
}) {
  return (
    <div className="upstream-notice" role="status" aria-live="polite">
      {chip.text && (
        <>
          <strong>{chip.title}</strong>
          <span>{chip.text}</span>
        </>
      )}
      {chip.progress && (
        <span className="upstream-progress">{chip.progress}</span>
      )}
    </div>
  );
}

/** Opens the file sidebar on the working directory's tree, or closes it
 * (docs/web-client.md "The file viewer"). */
function FilesToggle({ open, toggle }: { open: boolean; toggle: () => void }) {
  return (
    <button
      type="button"
      className="files-toggle"
      title={open ? "Close files" : "Files in the working directory"}
      aria-label="Files"
      aria-pressed={open}
      onClick={toggle}
    >
      <svg
        width="18"
        height="18"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v8.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z" />
      </svg>
    </button>
  );
}

export function TopBar({
  listHref,
  identity,
  usage,
  upstreamFailure,
  upstreamRetry,
  summary,
  files,
}: {
  /** The session list to go back to, when this page is served by a host —
   * `null` at a session daemon's root, where there is no list. */
  listHref: string | null;
  identity: SessionIdentity | null;
  usage: Usage | null;
  upstreamFailure: UpstreamFailure | null;
  upstreamRetry: UpstreamRetry | null;
  summary?: CompactControlProps;
  /** The working directory's file sidebar: whether it is open, and the toggle. */
  files?: { open: boolean; toggle: () => void };
}) {
  const chip = upstreamChip(upstreamFailure, upstreamRetry);
  const routeId = sessionId();
  const context = usage?.context ?? null;
  const cost = usage?.cost ?? null;
  return (
    <AppHeader
      homeHref={listHref}
      below={
        <>
          {listHref !== null && routeId !== null && (
            <SessionSwitcher currentId={routeId} />
          )}
          {chip !== null && <UpstreamNotice chip={chip} />}
        </>
      }
    >
      {(identity !== null || routeId !== null) && (
        <span className="session-name" title={identity?.id ?? routeId ?? ""}>
          {sessionName(identity, routeId)}
        </span>
      )}
      <span className="spacer" />
      {summary ? (
        <ContextPopup context={context} summary={summary}>
          {context !== null ? <ContextMeter context={context} /> : undefined}
        </ContextPopup>
      ) : (
        context !== null && <ContextMeter context={context} />
      )}
      {cost !== null && <CostMeter cost={cost} />}
      {usage?.plan && <PlanMeter plan={usage.plan} />}
      {files && <FilesToggle {...files} />}
      <AppSettingsLink />
    </AppHeader>
  );
}
