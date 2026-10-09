// The cold line at a decision (docs/web-client.md): while the `cold` topic is set, an
// approval gate and an `ask` say that the session's cache is believed expired and what
// the decision reads uncached. The line is also the condition for the consent: the
// decision carries `accept_cold_read` only when the line was showing, because a cold
// wait looks like a slow tool and a decision made without the notice is not consent
// (docs/control-api.md).

import type { Cold, Decision } from "../api/types.ts";
import { kilotokens } from "./fold.ts";

/** What the decision is, in the line's own words. A redirect is listed with
 * approving because its message is the next user message: the request that follows
 * reads the cache either way. */
export type ColdSubject = "approving or redirecting" | "your answer";

/** The one-line notice, or `null` while the cache is not believed expired. */
export function coldDecisionLine(
  cold: Cold | null,
  subject: ColdSubject,
): string | null {
  if (cold === null) return null;
  const read =
    cold.context_tokens !== null
      ? `~${kilotokens(cold.context_tokens)} tokens uncached`
      : "the context uncached";
  return `cache believed cold — ${subject} reads ${read}`;
}

/** Whether a decision made now carries the consent: exactly when the line is on
 * screen. */
export const consentsWhenShown = (line: string | null): boolean =>
  line !== null;

/** The gate decision as the client sends it: with `accept_cold_read` when the cold
 * line was showing. An abort is left alone; the host does not read the flag there. */
export function decisionWithConsent(
  decision: Decision,
  line: string | null,
): Decision {
  if (!consentsWhenShown(line) || decision.decision === "abort") {
    return decision;
  }
  return { ...decision, accept_cold_read: true };
}
