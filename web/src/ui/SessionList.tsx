// The host's root: the live sessions, each a link into its own view
// (docs/web-client.md).
//
// The list is a subscription, not a snapshot: `api/sessions.ts` holds the
// listing stream open, so a session created or ended anywhere appears or
// disappears here on its own. The rows carry what the listing carries — id,
// title, status, and the background processes a session holds — which is what
// lets a row say what it is doing without one `GET /sessions/<id>/v1/state` per
// session on every load.
//
// Each row is an `<a href>`: entering a session changes the control base, which
// is a navigation and not client-side state. Ending is the one thing here that
// is not a navigation, so it is a button, and a destructive one: it confirms in
// place when the row has something to lose.

import { useEffect, useState } from "react";
import { sessionHref } from "../api/base.ts";
import { openSessions } from "../api/sessions.ts";
import { endSession, type SessionEntry } from "../api/http.ts";
import { endWarning } from "./sessionRow.ts";

export function SessionList({
  sessions: initial,
}: {
  sessions: SessionEntry[];
}) {
  const [sessions, setSessions] = useState(initial);
  // The id whose end is waiting to be confirmed, and the id being ended. Both
  // are held here rather than per row so a second row's button cancels the
  // first's confirm rather than leaving two open. A confirm whose row has left
  // the listing simply stops matching any row, so it needs no cleanup.
  const [confirming, setConfirming] = useState<string | null>(null);
  const [ending, setEnding] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const handle = openSessions(
      (live) => {
        setSessions(live);
        setError(null);
      },
      () => setError("lost the connection to the host — retrying"),
    );
    return () => handle.close();
  }, []);

  async function end(id: string) {
    setConfirming(null);
    setEnding(id);
    try {
      await endSession(id);
      // The row leaves on the next frame of the listing stream; the host
      // answers only once teardown has run, so that frame is already out.
    } catch (failure) {
      setError(`ending ${id}: ${String(failure)}`);
    } finally {
      setEnding(null);
    }
  }

  return (
    <div className="app">
      <header className="top-bar">
        <div className="top-bar-inner">
          <span className="wordmark">efference</span>
          <span className="dim">sessions</span>
          <span className="spacer" />
        </div>
      </header>
      <div className="col session-list">
        {error !== null && <p className="dim">{error}</p>}
        {sessions.length === 0 ? (
          // Liveness, not existence: a dead session's log stays on disk and
          // readable by the offline verbs, and is not listed here.
          <p className="dim">
            no live sessions in this directory — start one with{" "}
            <code>efference serve</code>
          </p>
        ) : (
          <ul>
            {sessions.map((session) => {
              const warning = endWarning(session);
              return (
                <li key={session.id}>
                  <a href={sessionHref(session.id)}>
                    <span className="session-list-id">{session.id}</span>
                    <span className="session-list-status dim">
                      {session.status}
                      {session.processes > 0 && ` · ${session.processes} bg`}
                    </span>
                    {session.title !== undefined && (
                      <span className="session-list-title dim">
                        {session.title}
                      </span>
                    )}
                  </a>
                  {confirming === session.id ? (
                    <span className="session-list-confirm">
                      <span className="dim">
                        {warning === null
                          ? `end ${session.id}?`
                          : `${session.id} ${warning}. end it anyway?`}
                      </span>
                      <button
                        className="linkish"
                        onClick={() => void end(session.id)}
                      >
                        end
                      </button>
                      <button
                        className="linkish"
                        onClick={() => setConfirming(null)}
                      >
                        cancel
                      </button>
                    </span>
                  ) : (
                    <button
                      className="linkish session-list-end"
                      disabled={ending === session.id}
                      onClick={() =>
                        warning === null
                          ? void end(session.id)
                          : setConfirming(session.id)
                      }
                    >
                      {ending === session.id ? "ending…" : "end"}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
