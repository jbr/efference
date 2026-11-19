import type { ReactNode } from "react";
import "./ComposerReferences.css";

// Composer reference presentation (docs/web-client.md). The gallery exercises
// these components before the live editor supplies identities and actions.
export type ReferenceKind = "file" | "directory" | "agent" | "attachment";

export interface CompletionOption {
  id: string;
  label: string;
  detail: string;
  insertFolder?: boolean;
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

export function ReferenceChipFrame({
  label,
  kind,
  title,
  children,
  href,
  download,
  onClick,
}: {
  label: string;
  kind: ReferenceKind;
  title?: string;
  children?: ReactNode;
  href?: string;
  download?: string;
  onClick?: () => void;
}) {
  const props = { className: "reference-chip", title };
  const content = (
    <>
      <Kind kind={kind} />
      <span className="reference-chip-label">{label}</span>
      {children}
    </>
  );
  if (href !== undefined)
    return (
      <a {...props} href={href} download={download}>
        {content}
      </a>
    );
  if (onClick !== undefined)
    return (
      <button {...props} type="button" onClick={onClick}>
        {content}
      </button>
    );
  return <span {...props}>{content}</span>;
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
    <ReferenceChipFrame kind={kind} label={label} title={detail}>
      {filesystem && (
        <button
          type="button"
          className="reference-chip-access"
          title={write ? "Read and write" : "Read only"}
          aria-label={`Allow write access to ${label}`}
          aria-pressed={write}
          onClick={() => onWriteChange?.(!write)}
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.3"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            {write ? (
              <path d="m2.5 13.5 1-3.5L11 2.5a1 1 0 0 1 1.4 0l1.1 1.1a1 1 0 0 1 0 1.4L6 12.5z M10 3.5l2.5 2.5" />
            ) : (
              <>
                <path d="M1 8s2.5-4.5 7-4.5S15 8 15 8s-2.5 4.5-7 4.5S1 8 1 8Z" />
                <circle cx="8" cy="8" r="1.75" />
              </>
            )}
          </svg>
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
    </ReferenceChipFrame>
  );
}

function CompletionIcon({ kind }: { kind: ReferenceKind }) {
  if (kind !== "file" && kind !== "directory") return <Kind kind={kind} />;
  return (
    <svg
      className="reference-kind"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {kind === "directory" ? (
        <path d="M1.5 4h4.7l1.5 1.6h6.8v7.2h-13z" />
      ) : (
        <path d="M4 1.5h5l3 3v10H4z M9 1.5v3h3 M6 8h4 M6 10.5h4" />
      )}
    </svg>
  );
}

export function CompletionTray({ children }: { children: ReactNode }) {
  return (
    <div className="composer-completion-anchor col">
      <div className="composer-completion-popup">{children}</div>
    </div>
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
  const agents = (kind ?? options[0]?.kind) === "agent";
  return (
    <div className="reference-completion">
      {agents && <div className="reference-completion-heading">Sessions</div>}
      <div
        id={id}
        role="listbox"
        aria-label={agents ? "Sessions" : "Host files"}
        aria-busy={state === "loading"}
      >
        {options.map((option, index) => (
          <button
            type="button"
            role="option"
            id={`${id}-${index}`}
            key={option.id}
            aria-selected={selected === index}
            aria-label={
              option.insertFolder ? `${option.label}, insert folder` : undefined
            }
            className={`reference-completion-row selection-row${selected === index ? " selected" : ""}`}
            tabIndex={-1}
            onPointerMove={() => onHighlight(index)}
            onPointerDown={(event) => event.preventDefault()}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onAccept(index)}
          >
            <CompletionIcon kind={option.kind} />
            <span className="reference-completion-copy">
              {option.insertFolder ? (
                <bdi className="reference-completion-label reference-completion-folder-path">
                  {option.label}
                </bdi>
              ) : (
                <span className="reference-completion-label">
                  {option.label}
                </span>
              )}
              {agents && (
                <span className="reference-completion-detail">
                  {option.detail}
                </span>
              )}
            </span>
            {option.kind === "directory" && !option.insertFolder && (
              <span className="reference-completion-next" aria-hidden="true">
                ›
              </span>
            )}
          </button>
        ))}
        {state !== "ready" && (
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
    </div>
  );
}
