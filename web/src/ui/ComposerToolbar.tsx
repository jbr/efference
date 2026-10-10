import { turnClass, type TurnState } from "./turnState.ts";

/** The input's controls and readout, without session actions (docs/web-client.md). */
export function ComposerToolbar({
  turn,
  narrow,
  running,
  held = false,
  uploading = false,
  onAttach,
  onInterrupt,
  onResume,
}: {
  turn: TurnState;
  narrow: boolean;
  running: boolean;
  held?: boolean;
  uploading?: boolean;
  onAttach: () => void;
  onInterrupt: () => void;
  onResume: () => void;
}) {
  return (
    <div className="composer-toolbar">
      <div className="composer-toolbar-left">
        <button
          type="button"
          className="attach-btn"
          title="attach files (or paste, or drop them anywhere)"
          aria-label="attach files"
          onClick={onAttach}
        >
          <PaperClip />
          <span>attach</span>
        </button>
        <span className={turnClass(turn.kind)} title={turn.title}>
          {turn.text}
        </span>
      </div>
      <div className="composer-toolbar-right">
        {running && (
          <button type="button" className="interrupt-btn" onClick={onInterrupt}>
            interrupt
          </button>
        )}
        {held && (
          <button type="button" className="resume-btn" onClick={onResume}>
            resume
          </button>
        )}
        {!narrow && (
          <span className="composer-hint">shift-enter for newline</span>
        )}
        <button disabled={uploading} className="send-btn">
          <span>send</span>
          {!narrow && <kbd className="send-kbd">↵</kbd>}
        </button>
      </div>
    </div>
  );
}

export function PaperClip() {
  return (
    <svg
      width="16"
      height="16"
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
