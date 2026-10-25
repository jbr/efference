// Launch settings (docs/web-session-creation.md). Model controls open a role-specific
// page; effort and temperature commit overrides in place. Preview values never
// become overrides, and field errors stay beside the controls that fix them.
import type { ReactNode } from "react";
import type { SessionPreview } from "../api/sessionForm.ts";
import {
  EFFORTS,
  type Draft,
  modelText,
  temperatureOf,
  triageText,
} from "./sessionForm.ts";
import type { Role } from "./pickerRows.ts";
import { SelectionControl } from "./SelectionControl.tsx";
import "./NewSessionSettings.css";

export function Settings({
  draft,
  preview,
  onDraft,
  onPick,
}: {
  draft: Draft;
  preview: SessionPreview | null;
  onDraft: (draft: Draft) => void;
  onPick: (role: Role) => void;
}) {
  const effective = preview?.effective ?? null;
  const edit = (over: Partial<Draft>) => onDraft({ ...draft, ...over });
  const error = (field: string) =>
    preview?.issues.find((issue) => issue.field === field)?.message;
  if (effective === null)
    return (
      <p className="dim" role="status">
        Resolving settings…
      </p>
    );

  return (
    <section className="new-session-settings" aria-label="Session settings">
      <Setting
        label="Main model"
        error={error("model")}
        reset={draft.main !== null ? () => edit({ main: null }) : undefined}
      >
        <ModelControl
          label="Main model"
          value={modelText(effective.main.value)}
          onClick={() => onPick("main")}
        />
      </Setting>
      <Setting
        label="Effort"
        error={error("effort")}
        action={
          <button
            className="quiet effort-default"
            aria-label="Use default effort"
            aria-pressed={draft.effort === null}
            onClick={() => edit({ effort: null })}
          >
            Use default
          </button>
        }
      >
        <div className="effort-choices" role="group" aria-label="Effort levels">
          {EFFORTS.filter((level) => level.value !== null).map((level) => (
            <button
              key={level.label}
              aria-pressed={draft.effort === level.value}
              onClick={() => edit({ effort: level.value })}
            >
              {level.label}
            </button>
          ))}
        </div>
      </Setting>
      <Setting
        label="Temperature"
        error={error("temperature")}
        reset={
          draft.temperature !== null
            ? () => edit({ temperature: null })
            : undefined
        }
      >
        <TemperatureEditor
          value={draft.temperature}
          onCommit={(temperature) => edit({ temperature })}
        />
      </Setting>
      <Setting
        label="Spotter model"
        error={error("spotter_model")}
        reset={
          draft.spotter !== null ? () => edit({ spotter: null }) : undefined
        }
      >
        <ModelControl
          label="Spotter model"
          value={modelText(effective.spotter.value)}
          onClick={() => onPick("spotter")}
        />
      </Setting>
      <Setting
        label="Triage model"
        error={error("triage")}
        reset={
          draft.triage.mode !== "inherit"
            ? () => edit({ triage: { mode: "inherit" } })
            : undefined
        }
      >
        <ModelControl
          label="Triage model"
          value={
            effective.triage.value?.mode === "shared"
              ? "Same as main model"
              : triageText(effective.triage.value)
          }
          onClick={() => onPick("triage")}
        />
      </Setting>
      <Setting label="Worktree" error={error("worktree")}>
        <label className="worktree-toggle">
          <input
            type="checkbox"
            checked={draft.worktree}
            onChange={(event) =>
              edit({ worktree: event.currentTarget.checked })
            }
          />
          Start in its own worktree
        </label>
        <p className="worktree-description">
          Its own checkout and branch at{" "}
          <code>.efference/worktrees/&lt;id&gt;</code>.
        </p>
      </Setting>
    </section>
  );
}

export function Setting({
  label,
  children,
  error,
  reset,
  action,
}: {
  label: string;
  children: ReactNode;
  error?: string;
  reset?: () => void;
  action?: ReactNode;
}) {
  return (
    <div className="setting">
      <div className="setting-heading">
        <span className="setting-label">{label}</span>
        {action}
        {reset && (
          <button
            className="quiet setting-reset"
            aria-label={`Reset ${label}`}
            title="Clear this override"
            onClick={reset}
          >
            Reset
          </button>
        )}
      </div>
      {children}
      {error && <p className="new-session-issue">{error}</p>}
    </div>
  );
}

export function ModelControl({
  label,
  value,
  onClick,
}: {
  label: string;
  value: string | null;
  onClick: () => void;
}) {
  return (
    <SelectionControl
      label={`Choose ${label.toLowerCase()}: ${value ?? "Not selected"}`}
      onClick={onClick}
    >
      {value ?? "Choose a model"}
    </SelectionControl>
  );
}

// Commit on blur or Enter; resetting remounts the input without interrupting
// an edit with preview replies (docs/web-session-creation.md).
function TemperatureEditor({
  value,
  onCommit,
}: {
  value: number | null;
  onCommit: (value: number | null) => void;
}) {
  const commit = (
    event:
      | React.FocusEvent<HTMLInputElement>
      | React.KeyboardEvent<HTMLInputElement>,
  ) => {
    const next = temperatureOf(event.currentTarget.value);
    if (next !== value) onCommit(next);
  };
  return (
    <input
      key={`${value}`}
      className="setting-editor"
      type="number"
      aria-label="Temperature"
      min={0}
      max={100}
      placeholder="Use default"
      defaultValue={value ?? ""}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === "Enter") commit(event);
      }}
    />
  );
}
