// The cold line (docs/web-client.md): while the `cold` topic is set, an approval gate,
// an `ask` and the composer say that the session's cache is believed expired and what
// the next action reads uncached. It is information: the action goes through either
// way (docs/control-api.md).

import type { Cold } from "../api/types.ts";
import { kilotokens } from "./fold.ts";

/** What the action is, in the line's own words. A redirect is listed with
 * approving because its message is the next user message: the request that follows
 * reads the cache either way. */
export type ColdSubject = "approving or redirecting" | "your answer" | "a send";

/** The one-line notice, or `null` while the cache is not believed expired. */
export function coldLine(
  cold: Cold | null,
  subject: ColdSubject,
): string | null {
  if (cold === null) return null;
  const read =
    cold.context_tokens !== null
      ? `~${kilotokens(cold.context_tokens)} tokens uncached`
      : "the context uncached";
  return `cache expired — ${subject} reads ${read}`;
}
