// A session still being prepared, or one whose preparation failed
// (docs/web-client.md): what preparation is doing while the env hook and MCP
// mounts run, then why it stopped and the retry. The composer stays usable while
// starting — a turn sent then is queued and is the first the session runs.

import type { Starting } from "../api/types.ts";
import { startingLine } from "./startingLine.ts";

export function StartingBanner({
  starting,
  retrying,
  retryError,
  onRetry,
}: {
  starting: Starting;
  /** A retry is in flight. */
  retrying: boolean;
  /** Why the last retry was refused, when it was. */
  retryError: string | null;
  onRetry: () => void;
}) {
  if (starting.failure === null)
    return (
      <div className="banner" role="status">
        <div className="col">{startingLine(starting)}…</div>
      </div>
    );
  return (
    <div className="banner" role="alert">
      <div className="col">
        Did not start: {starting.failure.reason}{" "}
        <button type="button" disabled={retrying} onClick={onRetry}>
          {starting.resume ? "Retry" : "Back to the form"}
        </button>
        {retryError !== null && <p>{retryError}</p>}
      </div>
    </div>
  );
}
