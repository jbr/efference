// The top bar's transport chip (docs/web-client.md): one chip surface fed by
// two level-triggered topics. While `upstream_retry` is set it names the retry;
// otherwise `upstream_failure` names the last non-success status; with neither
// set there is no chip at all. Both topics clear on recovery, so the chip
// disappearing is the all-clear (docs/transport.md).

import type { UpstreamFailure, UpstreamRetry } from "../api/types.ts";

export interface UpstreamChip {
  /** What the chip reads. */
  text: string;
  /** The hover text: the upstream's own words for a retry, the chip's meaning
   * for a failure. */
  title: string;
  /** `true` while a retry is in flight — the chip is describing something in
   * progress rather than a status already received. */
  retrying: boolean;
}

/** The retry's lead line, with a fork peek marked apart from a turn: an
 * exhausted peek withholds a verdict where an exhausted turn ends the
 * trajectory, so the two are not the same event to someone watching a run. */
export function retryText(retry: UpstreamRetry): string {
  return retry.surface === "turn"
    ? `↻ ${retry.message}`
    : `↻ ${retry.surface}: ${retry.message}`;
}

/**
 * The chip for the current pair of topic values, or `null` for no chip.
 *
 * A pure function of the two topics: no clock is read. The topic carries
 * `next_attempt_at` as an absolute instant, and rendering the remaining wait
 * would mean either a `Date.now()` in the render path or an interval driving a
 * countdown — the attempt/budget the message already names is the part that
 * says how far along the retry is.
 */
export function upstreamChip(
  failure: UpstreamFailure | null,
  retry: UpstreamRetry | null,
): UpstreamChip | null {
  if (retry !== null) {
    return { text: retryText(retry), title: retry.detail, retrying: true };
  }
  if (failure !== null) {
    return {
      text: `upstream ${failure.status}${failure.count > 1 ? ` ×${failure.count}` : ""}`,
      title:
        "the last non-success upstream response; cleared by the next success",
      retrying: false,
    };
  }
  return null;
}
