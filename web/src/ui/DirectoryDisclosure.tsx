// Inline directory entry (docs/web-session-creation.md).
import { useEffect, useRef } from "react";

export function DirectoryDisclosure({
  open,
  value,
  onToggle,
  onChange,
  onCommit,
}: {
  open: boolean;
  value: string;
  onToggle: () => void;
  onChange: (value: string) => void;
  onCommit: () => void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(open);
  useEffect(() => {
    if (wasOpen.current && !open) trigger.current?.focus();
    wasOpen.current = open;
  }, [open]);
  return (
    <div className="new-session-elsewhere">
      <button
        ref={trigger}
        type="button"
        className="location-pick selection-row directory-disclosure"
        aria-expanded={open}
        aria-controls="launch-directory-form"
        onClick={onToggle}
      >
        <span className="location-line">
          <span className="location-path location-name">Somewhere else…</span>
          <span className="location-next" aria-hidden="true">
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              className={
                open ? "disclosure-chevron open" : "disclosure-chevron"
              }
            >
              <path d="m6 9 6 6 6-6" />
            </svg>
          </span>
        </span>
      </button>
      {open && (
        <form
          id="launch-directory-form"
          className="new-session-other"
          onSubmit={(event) => {
            event.preventDefault();
            onCommit();
          }}
        >
          <label htmlFor="launch-directory">Directory on the host</label>
          <input
            id="launch-directory"
            autoFocus
            type="text"
            value={value}
            placeholder="Full directory path"
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            onChange={(event) => onChange(event.target.value)}
          />
          <p className="dim">
            The path is on the machine running efference, not necessarily this
            device.
          </p>
          <div className="new-session-other-actions">
            <button type="button" onClick={onToggle}>
              Close
            </button>
            <button className="primary" type="submit">
              Use this directory
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
