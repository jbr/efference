// The entry: which of the two things this page is (session/entry.ts). One
// session's view is the answer under a session prefix and at a session daemon's
// root; the host's root is the list of live sessions instead. Moving between
// them, or from one session to another, is a client-side navigation
// (ui/route.ts): the view is keyed by the session, so each session gets its own
// component state, while its store comes from the warm registry
// (session/registry.ts) and renders the last snapshot at once
// (docs/web-client.md).

import { deriveBase } from "../api/base.ts";
import { useEntry } from "../session/entry.ts";
import { APP_TITLE, useDocumentTitle } from "./documentTitle.ts";
import { Login } from "./Login.tsx";
import { usePathname } from "./route.ts";
import { SessionList } from "./SessionList.tsx";
import { SessionView } from "./SessionView.tsx";

export function App() {
  const base = deriveBase(usePathname());
  const { entry, resolve } = useEntry(base);
  useDocumentTitle(entry?.kind === "session" ? null : APP_TITLE);
  // The probe is one same-origin request; the shell paints, and this is the
  // moment before it answers. Nothing is claimed about the binding until it does.
  if (entry === null) return null;
  if (entry.kind === "unauthenticated") {
    return <Login onAuthenticated={resolve} />;
  }
  if (entry.kind === "list") {
    // The probe's answer seeds the list; the list holds the listing stream open
    // from there, so nothing above it re-resolves (docs/web-client.md).
    return <SessionList sessions={entry.sessions} />;
  }
  return <SessionView key={base} />;
}
