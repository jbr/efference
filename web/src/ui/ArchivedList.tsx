import { RelativeTime } from "./RelativeTime.tsx";
import type { ArchivedSession } from "../api/http.ts";

export function ArchivedList({
  sessions,
  remainderCount,
  resuming,
  onResume,
  onOpenPicker,
}: {
  sessions: ArchivedSession[];
  remainderCount: number;
  resuming: string | null;
  onResume: (id: string) => void;
  onOpenPicker: () => void;
}) {
  if (sessions.length === 0 && remainderCount === 0) {
    return null;
  }

  return (
    <div className="archived-workspace-section">
      <div className="archived-workspace-head">
        <span className="archived-workspace-label">Recent</span>
      </div>
      <div className="archived-table">
        {sessions.map((session) => {
          const isResuming = resuming === session.id;

          return (
            <div className="archived-row" key={session.id}>
              <span
                className="archived-row-title"
                title={session.title ?? session.id}
              >
                {session.title ?? (
                  <span className="archived-row-untitled">Untitled</span>
                )}
              </span>
              <span className="archived-row-id">{session.id}</span>
              <span className="archived-row-age">
                {session.last_active_at ? (
                  <RelativeTime date={session.last_active_at} />
                ) : null}
              </span>
              <button
                className="archived-row-resume"
                onClick={() => onResume(session.id)}
                disabled={isResuming}
                aria-label={`Resume ${session.id}`}
              >
                {isResuming ? "Resuming…" : "Resume"}
              </button>
            </div>
          );
        })}
      </div>
      {remainderCount > 0 && (
        <button className="archived-more-button" onClick={onOpenPicker}>
          › {remainderCount} more archived{" "}
          {remainderCount === 1 ? "session" : "sessions"}…
        </button>
      )}
    </div>
  );
}
