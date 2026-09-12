// The host's root: the live sessions, each a link into its own view
// (docs/web-client.md). A row carries the id and the title, because that is
// what the listing carries — a session's state lives behind its own API, so
// any other chip per row would be one `GET /sessions/<id>/v1/state` per
// session on every load. The cross-session summary a list actually wants is a host-level
// resource that is not built (backlog/host-process.md); until it is, the honest
// list is the one the endpoint can answer.
//
// Each row is an `<a href>`: entering a session changes the control base, which
// is a navigation and not client-side state.

import { sessionHref } from "../api/base.ts";
import type { SessionEntry } from "../api/http.ts";

export function SessionList({
  sessions,
  onReload,
}: {
  sessions: SessionEntry[];
  onReload: () => void;
}) {
  return (
    <div className="app">
      <header className="top-bar">
        <div className="top-bar-inner">
          <span className="wordmark">efference</span>
          <span className="dim">sessions</span>
          <span className="spacer" />
          <button className="linkish" onClick={onReload}>
            reload
          </button>
        </div>
      </header>
      <div className="col session-list">
        {sessions.length === 0 ? (
          // Liveness, not existence: a dead session's log stays on disk and
          // readable by the offline verbs, and is not listed here.
          <p className="dim">
            no live sessions in this directory — start one with{" "}
            <code>efference serve</code>
          </p>
        ) : (
          <ul>
            {sessions.map((session) => (
              <li key={session.id}>
                <a href={sessionHref(session.id)}>
                  <span className="session-list-id">{session.id}</span>
                  {session.title !== undefined && (
                    <span className="session-list-title dim">
                      {session.title}
                    </span>
                  )}
                </a>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
