import type { StartingView } from "../api/types.ts";
import type { Connection } from "../session/store.ts";
import type { ComposerStatusMessage } from "./ComposerStatus.tsx";
import { startingLine } from "./startingLine.ts";

/** Session conditions replace the normal turn readout (docs/web-client.md). */
export function sessionStatus({
  starting,
  connection,
  retrying,
  retryError,
  onRetry,
  inspectionError,
}: {
  starting: StartingView | null;
  connection: Connection;
  retrying: boolean;
  retryError: string | null;
  onRetry: () => void;
  inspectionError: string | null;
}): ComposerStatusMessage | null {
  if (starting?.failure)
    return {
      text: retrying ? "retrying startup…" : "Could not start",
      error: true,
      detail: [starting.failure.reason, retryError].filter(Boolean).join("\n"),
      action: {
        label: retrying
          ? "Retrying…"
          : starting.resume
            ? "Retry"
            : "Back to the form",
        pending: retrying,
        onClick: onRetry,
      },
    };
  if (inspectionError)
    return { text: `Could not inspect task: ${inspectionError}`, error: true };
  if (starting) return { text: `${startingLine(starting)}…` };
  if (connection !== "live") return { text: `${connection}…` };
  return null;
}
