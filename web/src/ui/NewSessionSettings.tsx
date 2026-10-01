// The Settings section of the new-session form (docs/web-client.md): what a
// session would actually get, field by field, and the two fields this step can
// edit. Every row shows its value *and* the branch that supplied it, because an
// inherited default and an explicit override are the same string otherwise —
// "your default" says the operator did not choose it, and the row is editable in
// place either way.
//
// Rows are the terminal form's in its order ([tui.md](tui.md)). The three model
// rows read the host's answer and are not editable yet: choosing one is the
// model picker, which is the next step (backlog/new-session-form.md). Effort and
// temperature are the editors this step owns.
//
// A field with an issue shows the issue's message *in place of* its source, in
// the warn colour. A field that failed has neither a value nor a source
// (docs/control-api.md), so there is nothing for the source column to say.

import type { SessionPreview } from "../api/sessionForm.ts";
import {
  EFFORTS,
  type Draft,
  fieldSource,
  hasIssue,
  modelText,
  temperatureOf,
  triageText,
} from "./sessionForm.ts";
import "./NewSessionSettings.css";

export function Settings({
  draft,
  preview,
  onDraft,
}: {
  draft: Draft;
  /** The host's latest answer, or `null` while none has landed for the draft on
   * screen. */
  preview: SessionPreview | null;
  onDraft: (draft: Draft) => void;
}) {
  const effective = preview?.effective ?? null;
  const issues = preview?.issues ?? [];
  const edit = (over: Partial<Draft>) => onDraft({ ...draft, ...over });

  if (effective === null) {
    return (
      <section className="new-session-settings">
        <h3>Settings</h3>
        <p className="dim">Resolving…</p>
      </section>
    );
  }

  return (
    <section className="new-session-settings">
      <h3>Settings</h3>

      <Row
        label="Main model"
        value={modelText(effective.main.value)}
        source={fieldSource("model", effective.main.source, issues)}
        failed={hasIssue(issues, "model")}
      />

      <Row
        label="Effort"
        source={fieldSource("effort", effective.effort.source, issues)}
        failed={hasIssue(issues, "effort")}
      >
        <select
          className="setting-editor"
          aria-label="Effort"
          value={draft.effort ?? ""}
          onChange={(event) => edit({ effort: event.target.value || null })}
        >
          {EFFORTS.map((level) => (
            <option key={level.label} value={level.value ?? ""}>
              {level.label}
            </option>
          ))}
        </select>
        {draft.effort !== null && (
          <Reset onClick={() => edit({ effort: null })} label="Effort" />
        )}
      </Row>

      <Row
        label="Temperature"
        source={fieldSource(
          "temperature",
          effective.temperature.source,
          issues,
        )}
        failed={hasIssue(issues, "temperature")}
      >
        <TemperatureEditor
          value={draft.temperature}
          onCommit={(temperature) => edit({ temperature })}
        />
        {draft.temperature !== null && (
          <Reset
            onClick={() => edit({ temperature: null })}
            label="Temperature"
          />
        )}
      </Row>

      <Row
        label="Spotter model"
        value={modelText(effective.spotter.value)}
        source={fieldSource("spotter_model", effective.spotter.source, issues)}
        failed={hasIssue(issues, "spotter_model")}
      />

      <Row
        label="Triage model"
        value={triageText(effective.triage.value)}
        source={fieldSource("triage", effective.triage.source, issues)}
        failed={hasIssue(issues, "triage")}
      />
    </section>
  );
}

/** One row: the field's name, its value, and where that value came from — or
 * the issue that stopped it resolving, which takes the source's place. */
function Row({
  label,
  value,
  source,
  failed,
  children,
}: {
  label: string;
  /** The value as shown. Absent where the row's own editor is the value. */
  value?: string | null;
  source: string | null;
  /** A field that failed to resolve has no value at all, which reads
   * differently from one nothing named. */
  failed?: boolean;
  children?: React.ReactNode;
}) {
  const warn = failed === true;
  return (
    <div className={warn ? "setting warn" : "setting"}>
      <span className="setting-label">{label}</span>
      <span className="setting-value">
        {children ??
          (value === null || value === undefined ? (
            <span className="setting-empty">—</span>
          ) : (
            // A model slug is longer than its column in most connections, so
            // the whole one is in the title rather than only its start.
            <code title={value}>{value}</code>
          ))}
      </span>
      {source !== null && (
        <span className={warn ? "setting-source warn" : "setting-source"}>
          {source}
        </span>
      )}
    </div>
  );
}

/** Clears one field's override and previews again — the same verb the terminal
 * form's `r` is, and its only reset: there is no inherit row, because the
 * reset *is* the inherit. */
function Reset({ onClick, label }: { onClick: () => void; label: string }) {
  return (
    <button
      className="setting-reset"
      title="Clear this override"
      aria-label={`Reset ${label}`}
      onClick={onClick}
    >
      reset
    </button>
  );
}

/** Temperature is committed rather than sent per keystroke: the preview is
 *  recomputed on committed edits (docs/tui.md), so what the field holds is the
 *  draft only once the operator leaves it or presses Enter.
 *
 *  Uncontrolled, keyed on the draft: the field owns the text while it is being
 *  typed, and a draft that moved under it — a reset — remounts it at the new
 *  value rather than mirroring state it is in the middle of editing. */
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
