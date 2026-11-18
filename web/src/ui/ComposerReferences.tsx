import { useId } from "react";
import "./ComposerReferences.css";

// Composer reference presentation (docs/web-client.md). The gallery exercises
// these components before the live editor supplies identities and actions.
export type ReferenceKind = "file" | "directory" | "agent" | "attachment";

export interface CompletionOption {
  id: string;
  label: string;
  detail: string;
  kind: ReferenceKind;
}

function Kind({ kind }: { kind: ReferenceKind }) {
  return (
    <span className="reference-kind" aria-hidden="true">
      {kind === "directory"
        ? "▱"
        : kind === "agent"
          ? "@"
          : kind === "attachment"
            ? "↗"
            : "≡"}
    </span>
  );
}

export function ReferenceChip({
  label,
  detail,
  kind,
  write = false,
  onWriteChange,
  onRemove,
}: {
  label: string;
  detail: string;
  kind: ReferenceKind;
  write?: boolean;
  onWriteChange?: (write: boolean) => void;
  onRemove?: () => void;
}) {
  const filesystem = kind === "file" || kind === "directory";
  return (
    <span className="reference-chip" title={detail}>
      <Kind kind={kind} />
      <span className="reference-chip-label">{label}</span>
      {filesystem && (
        <button
          type="button"
          className="reference-chip-access"
          aria-label={`Allow write access to ${label}`}
          aria-pressed={write}
          onClick={() => onWriteChange?.(!write)}
        >
          {write ? "read + write" : "read"}
        </button>
      )}
      <button
        type="button"
        className="reference-chip-remove"
        aria-label={`Remove ${label}`}
        onClick={onRemove}
      >
        ×
      </button>
    </span>
  );
}

export function CompletionMenu({
  id,
  options,
  selected,
  onHighlight,
  onAccept,
  state = "ready",
  kind,
}: {
  kind?: ReferenceKind;
  id: string;
  options: CompletionOption[];
  selected: number;
  onHighlight: (index: number) => void;
  onAccept: (index: number) => void;
  state?: "ready" | "loading" | "empty" | "error";
}) {
  const caption = useId();
  const agents = (kind ?? options[0]?.kind) === "agent";
  return (
    <div className="reference-completion">
      <div className="reference-completion-heading" id={caption}>
        {agents ? "Sessions" : "Files and directories"}
        <span>reference completion</span>
      </div>
      <div
        id={id}
        role="listbox"
        aria-labelledby={caption}
        aria-busy={state === "loading"}
      >
        {state === "ready" ? (
          options.map((option, index) => (
            <button
              type="button"
              role="option"
              id={`${id}-${index}`}
              key={option.id}
              aria-selected={selected === index}
              className="reference-completion-row"
              tabIndex={-1}
              onPointerMove={() => onHighlight(index)}
              onPointerDown={(event) => event.preventDefault()}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => onAccept(index)}
            >
              <Kind kind={option.kind} />
              <span className="reference-completion-copy">
                <span className="reference-completion-label">
                  {option.label}
                </span>
                <span className="reference-completion-detail">
                  {option.detail}
                </span>
              </span>
              <span className="reference-completion-action">
                {option.kind === "directory" ? "directory" : option.kind}
              </span>
            </button>
          ))
        ) : (
          <p className="reference-completion-note" role="status">
            {state === "loading"
              ? "Looking up candidates…"
              : state === "empty"
                ? agents
                  ? "No other sessions match. Keep typing to change the search."
                  : "No matches. Keep typing to change the path."
                : agents
                  ? "Could not list sessions. Your draft is unchanged."
                  : "Could not list this location. Your draft is unchanged."}
          </p>
        )}
      </div>
      <div className="reference-completion-help">
        <span>
          <kbd>↑</kbd>
          <kbd>↓</kbd> browse
        </span>
        <span>
          <kbd>Tab</kbd> / <kbd>Enter</kbd>{" "}
          {options[selected]?.kind === "directory"
            ? "reference directory"
            : "reference"}
        </span>
        <span>
          <kbd>Esc</kbd> dismiss
        </span>
      </div>
    </div>
  );
}
