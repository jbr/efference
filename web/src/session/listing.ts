// The last listing of the host's sessions that any view heard
// (docs/web-client.md). Switching sessions remounts the session view, and with
// it the switcher and the header; what this holds keeps the tab row painted and
// the new session's title known through that, until the fresh subscription's
// first frame corrects it. A seed, not a source of truth.

import type { SessionEntry } from "../api/http.ts";

let listing: SessionEntry[] | null = null;

export const lastListing = (): SessionEntry[] | null => listing;

export function rememberListing(next: SessionEntry[]): void {
  listing = next;
}

/** The title the listing last gave `id`, if it has given one. */
export const listedTitle = (id: string): string | null =>
  listing?.find((entry) => entry.id === id)?.title ?? null;
