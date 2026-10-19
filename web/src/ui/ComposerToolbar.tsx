import type { TurnState } from "./turnState.ts";
import {
  ComposerStatus,
  type ComposerStatusMessage,
} from "./ComposerStatus.tsx";
import "./ComposerToolbar.css";

/** The input's controls and readout, without session actions (docs/web-client.md). */
export function ComposerToolbar({
  turn,
  status,
  narrow,
  running,
  held = false,
  holdPending = false,
  uploading = false,
  onAttach,
  onInterrupt,
  onHoldChange,
}: {
  turn: TurnState;
  status?: ComposerStatusMessage | null;
  narrow: boolean;
  running: boolean;
  held?: boolean;
  holdPending?: boolean;
  uploading?: boolean;
  onAttach: () => void;
  onInterrupt: () => void;
  onHoldChange: (held: boolean) => void;
}) {
  return (
    <div className="composer-toolbar">
      <div className="composer-toolbar-left">
        <div className="composer-toolbar-tools">
          <button
            type="button"
            className="attach-btn composer-icon-button"
            title="Attach files (or paste, or drop them anywhere)"
            aria-label="Attach files"
            onClick={onAttach}
          >
            <PaperClip />
          </button>
          <HoldButton
            held={held}
            pending={holdPending}
            onChange={onHoldChange}
          />
        </div>
        <ComposerStatus turn={turn} message={status} />
      </div>
      <div className="composer-toolbar-right">
        {running && (
          <button
            type="button"
            className="interrupt-btn composer-icon-button"
            title="Interrupt current operation"
            aria-label="Interrupt current operation"
            onClick={onInterrupt}
          >
            <ComposerActionIcon kind="stop" />
          </button>
        )}
        {!narrow && (
          <span className="composer-hint">shift-enter for newline</span>
        )}
        <button
          type="submit"
          disabled={uploading}
          className="send-btn composer-icon-button"
          title={narrow ? "Send message" : "Send message (Enter)"}
          aria-label="Send message"
        >
          <ComposerActionIcon kind="send" />
        </button>
      </div>
    </div>
  );
}

export function HoldButton({
  held,
  pending = false,
  onChange,
}: {
  held: boolean;
  pending?: boolean;
  onChange: (held: boolean) => void;
}) {
  const label = held ? "Release hold" : "Hold after the current step";
  return (
    <button
      type="button"
      className="hold-btn composer-icon-button"
      title={label}
      aria-label={label}
      aria-pressed={held}
      aria-busy={pending}
      disabled={pending}
      onClick={() => onChange(!held)}
    >
      <ComposerActionIcon kind={held ? "play" : "pause"} />
    </button>
  );
}

export function ComposerActionIcon({
  kind,
}: {
  kind: "send" | "play" | "pause" | "stop";
}) {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {kind === "send" ? (
        <path d="M12 20V4m-7 7 7-7 7 7" />
      ) : kind === "pause" ? (
        <path d="M8 5v14M16 5v14" />
      ) : kind === "play" ? (
        <path d="m8 5 11 7-11 7Z" />
      ) : (
        <path d="M8 3h8l5 5v8l-5 5H8l-5-5V8Z" />
      )}
    </svg>
  );
}

export function PaperClip() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
    </svg>
  );
}
