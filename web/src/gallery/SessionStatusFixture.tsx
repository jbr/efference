import { useState } from "react";
import type { Starting } from "../api/types.ts";
import { ComposerToolbar } from "../ui/ComposerToolbar.tsx";
import { sessionStatus } from "../ui/sessionStatus.ts";
import { turnState } from "../ui/turnState.ts";
import { useNarrow } from "../ui/useNarrow.ts";
import type { StatusState } from "./SessionChromeSpecimens.tsx";

/** Production status and controls, with local-only actions (docs/web-client.md). */
export function SessionStatusFixture({ state }: { state: StatusState }) {
  const [retrying, setRetrying] = useState(false);
  const [held, setHeld] = useState(false);
  const [notice, setNotice] = useState("");
  const narrow = useNarrow();
  const failed = state === "Startup failed" || state === "Retry refused";
  const inspection = state === "Inspection failed";
  const starting: Starting | null =
    state === "Your turn" || state === "Reconnecting" || inspection
      ? null
      : {
          id: "deft-walnut",
          cwd: "/Users/jbr/code/efference",
          since: 0,
          resume: true,
          stage: state === "Environment" ? "environment" : "preparing",
          authorizing: state === "Authorization" ? "github" : null,
          failure: failed
            ? {
                reason:
                  "MCP server ‘structural’ did not respond during initialization. Check its command and environment, then retry.",
                at: 0,
                withdrawn: [],
              }
            : null,
        };
  const status = sessionStatus({
    starting,
    connection: state === "Reconnecting" ? "reconnecting" : "live",
    retrying,
    retryError:
      state === "Retry refused"
        ? "The host is unavailable. Reconnect before retrying."
        : null,
    onRetry: () => setRetrying(true),
    inspectionError: inspection ? "the task is no longer available" : null,
  });
  return (
    <form
      className="composer g-composer"
      onSubmit={(event) => {
        event.preventDefault();
        setNotice("Gallery only — nothing was sent.");
      }}
    >
      <div className="composer-inner">
        <div className="composer-input">
          <div className="composer-text-row">
            <textarea
              aria-label="Session status draft"
              placeholder="send a turn…"
              rows={3}
            />
          </div>
          <ComposerToolbar
            turn={turnState(
              inspection ? { phase: "requesting" } : { phase: "idle" },
            )}
            status={status}
            narrow={narrow}
            running={inspection}
            held={held}
            onHoldChange={setHeld}
            onAttach={() => setNotice("Gallery only — no file was uploaded.")}
            onInterrupt={() =>
              setNotice("Gallery only — no session was interrupted.")
            }
          />
        </div>
        {notice && (
          <p className="g-caption" role="status">
            {notice}
          </p>
        )}
      </div>
    </form>
  );
}
