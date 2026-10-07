import { RelativeTime } from "./RelativeTime.tsx";
import type { ArchivedSession } from "../api/http.ts";

export function ArchivedCard({
  session,
  resuming,
  onResume,
}: {
  session: ArchivedSession;
  resuming: boolean;
  onResume: () => void;
}) {
  return (
    <article className={`card card-archived${resuming ? " resuming" : ""}`}>
      <header className="card-head">
        <span className="card-status-line">
          <span className="card-id">{session.id}</span>
          <span className="card-status"> is archived</span>
        </span>
      </header>

      <h3 className="card-title">
        <button
          className="card-link"
          onClick={onResume}
          disabled={resuming}
          aria-label={`Resume session ${session.id}`}
        >
          {session.title ?? <span className="card-title-new">Untitled</span>}
        </button>
      </h3>

      <footer className="card-meta">
        <div className="card-line">
          <span className="card-archived-age">
            {session.last_active_at ? (
              <>
                active <RelativeTime date={session.last_active_at} />
              </>
            ) : null}
          </span>
          <span className="resume-hint">
            {resuming ? "resuming…" : "resume ›"}
          </span>
        </div>
      </footer>
    </article>
  );
}
