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
          {session.last_active_at && (
            <span className="card-status">
              {" "}
              · archived <RelativeTime date={session.last_active_at} />
            </span>
          )}
        </span>
        <span className="spacer" />
        <button
          className="resume-action-button"
          onClick={onResume}
          disabled={resuming}
          aria-label={`Resume session ${session.id}`}
        >
          {resuming ? "Resuming…" : "Resume"}
        </button>
      </header>

      <h3 className="card-title">
        <span className="card-title-text">
          {session.title ?? <span className="card-title-new">Untitled</span>}
        </span>
      </h3>

      <div className="card-archived-meta">
        <span className="dim">Archived session</span>
      </div>
    </article>
  );
}
