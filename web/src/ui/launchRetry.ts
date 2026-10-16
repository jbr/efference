// A creation's way back to the form when its start fails
// (docs/web-session-creation.md). `POST /sessions` answers once the session is
// reserved, so a failure arrives on the session's own view, after the form has
// gone. The draft that made each session is kept here by its id; a retry reopens
// the form on it with the reason, and the session the retry makes receives the
// failed view's composer text — the turns its start handed back.

import { acquireDraft } from "./drafts.ts";
import type { Draft } from "./sessionForm.ts";

const launched = new Map<string, Draft>();
let reopened: { draft: Draft | null; reason: string; carried: string } | null =
  null;

/** The form made session `id` from `draft`. */
export function rememberLaunch(id: string, draft: Draft): void {
  launched.set(id, draft);
}

/** Reopen the form on the draft that made `id` — none when this page did not
 * create it, and the form starts at the location — saying `reason`, with
 * `carried` bound for the next session it makes. */
export function reopenLaunch(id: string, reason: string, carried: string) {
  const draft = launched.get(id) ?? null;
  launched.delete(id);
  reopened = { draft, reason, carried };
}

/** The draft and reason the form reopens on, when a retry asked for it. */
export function reopenedLaunch(): {
  draft: Draft | null;
  reason: string;
} | null {
  return reopened;
}

/** The form made `id`: a retry's carried text goes into its composer, and the
 * reopening is done. */
export function launchedFrom(id: string, draft: Draft): void {
  rememberLaunch(id, draft);
  if (reopened !== null && reopened.carried !== "") {
    acquireDraft(`/sessions/${id}`).setText(reopened.carried);
  }
  reopened = null;
}
