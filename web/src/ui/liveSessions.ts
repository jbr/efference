// The host's live sessions, followed (docs/web-client.md): the switcher's tabs
// and the session view's own row read the same stream, and the listing is
// remembered so a second reader paints its first frame from the last one.

import { useEffect, useState } from "react";
import { apiBase } from "../api/base.ts";
import type { SessionEntry } from "../api/http.ts";
import { openSessions } from "../api/sessions.ts";
import { lastListing, rememberListing } from "../session/listing.ts";

/** `null` until the first listing, and always `null` where the page has no host
 * behind it. */
export function useLiveSessions(): SessionEntry[] | null {
  const [sessions, setSessions] = useState<SessionEntry[] | null>(
    lastListing(),
  );
  useEffect(() => {
    if (apiBase() === "") return;
    const handle = openSessions((listing) => {
      rememberListing(listing);
      setSessions(listing);
    });
    return () => handle.close();
  }, []);
  return sessions;
}
