import { RelativeTime } from "./RelativeTime.tsx";
import { ActionsMenu } from "./ActionsMenu.tsx";
import type { ArchivedSession } from "../api/http.ts";

export function ArchivedCard({
  session,
  resuming,
  onResume,
  onSwitch,
}: {
  session: ArchivedSession;
  resuming: boolean;
  onResume: () => void;
  /** Open the model switch (ui/SwitchModelPage.tsx): the resume then names the
   * models, rather than taking the log's. */
  onSwitch: () => void;
}) {
  return (
    <article className={`card card-archived${resuming ? " resuming" : ""}`}>
      <header className="card-head">
        <span className="card-status-line">
          <span className="card-id">{session.id}</span>
          <span className="card-status"> is archived</span>
        </span>
        <span className="spacer" />
        <ActionsMenu label={`actions for ${session.id}`}>
          {(close) => (
            <button
              role="menuitem"
              onClick={() => {
                close();
                onSwitch();
              }}
            >
              Switch model…
            </button>
          )}
        </ActionsMenu>
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
        {session.resume_failure !== undefined && (
          <div className="card-line" role="note">
            last resume did not start: {session.resume_failure}
          </div>
        )}
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
