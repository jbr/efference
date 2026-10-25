import { RelativeTime } from "./RelativeTime.tsx";
import { ActionsMenu } from "./ActionsMenu.tsx";
import type { ArchivedSession } from "../api/http.ts";

export function ArchivedList({
  sessions,
  remainderCount,
  resuming,
  onResume,
  onSwitch,
  onOpenPicker,
}: {
  sessions: ArchivedSession[];
  remainderCount: number;
  resuming: string | null;
  onResume: (id: string) => void;
  /** Open the model switch (ui/SwitchModelPage.tsx) for a session that is not
   * running: it resumes on the chosen models. */
  onSwitch: (id: string) => void;
  onOpenPicker: () => void;
}) {
  if (sessions.length === 0 && remainderCount === 0) {
    return null;
  }

  return (
    <div className="archived-workspace-section">
      <div className="archived-workspace-head">recent</div>
      <div className="archived-table">
        {sessions.map((session, index) => {
          const isResuming = resuming === session.id;
          // The table clips what leaves it, and a menu opening downward would
          // cover the more-button, so the last row opens upward.
          const atBottom = index === sessions.length - 1;

          return (
            <div className="archived-row-wrap" key={session.id}>
              <button
                className="archived-row"
                onClick={() => onResume(session.id)}
                disabled={isResuming}
                aria-label={`Resume ${session.id}`}
              >
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
                <span className="resume-hint">
                  {isResuming ? "resuming…" : "resume ›"}
                </span>
              </button>
              <ActionsMenu
                label={`actions for ${session.id}`}
                align={atBottom ? "up" : "down"}
              >
                {(close) => (
                  <button
                    role="menuitem"
                    onClick={() => {
                      close();
                      onSwitch(session.id);
                    }}
                  >
                    Switch model…
                  </button>
                )}
              </ActionsMenu>
            </div>
          );
        })}
        {remainderCount > 0 && (
          <button className="archived-more-button" onClick={onOpenPicker}>
            › {remainderCount} more archived{" "}
            {remainderCount === 1 ? "session" : "sessions"}…
          </button>
        )}
      </div>
    </div>
  );
}
