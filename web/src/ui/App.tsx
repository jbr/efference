// The entry: which of the two things this page is (session/entry.ts). One
// session's view is the answer under a session prefix and at a session daemon's
// root; the host's root is the list of live sessions instead, and entering one
// from it is a navigation — the base changes, so the page loads again against a
// different control endpoint (docs/host.md). Nothing here is a router: there is
// no client-side transition between the two, because there is no shared client
// state that could survive one.

import { useEntry } from "../session/entry.ts";
import { Login } from "./Login.tsx";
import { SessionList } from "./SessionList.tsx";
import { SessionView } from "./SessionView.tsx";

export function App() {
  const { entry, resolve } = useEntry();
  // The probe is one same-origin request; the shell paints, and this is the
  // moment before it answers. Nothing is claimed about the binding until it does.
  if (entry === null) return null;
  if (entry.kind === "unauthenticated") {
    return <Login onAuthenticated={resolve} />;
  }
  if (entry.kind === "list") {
    return <SessionList sessions={entry.sessions} onReload={resolve} />;
  }
  return <SessionView />;
}
