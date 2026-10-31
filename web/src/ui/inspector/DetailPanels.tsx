// The inspector's Details panels (docs/web-client.md): the harness's
// measurement of a call, and the triage verdict's account of its result.

import type {
  Keep,
  SelectionLines,
  ToolOutcome,
  ToolTelemetry,
  WireState,
} from "../../api/types.ts";
import type { RawResult } from "../../api/entry.ts";
import { wireLabel } from "../../render/parse.ts";
import { rustLines } from "../../render/triage.ts";

const keepLabel = (keep: Keep): string => {
  if (keep === "All") return "All";
  if (keep === "None") return "None";
  return `Ranges(${keep.Ranges.map(([a, b]) => `${a}..${b}`).join(", ")})`;
};

const surfacedLines = (keep: Keep, total: number): number => {
  if (keep === "All") return total;
  if (keep === "None") return 0;
  return keep.Ranges.reduce((n, [a, b]) => n + (b - a + 1), 0);
};

/** "What the fork returned": the verdict's account of the result — raw size,
 * what survived, the keep, the wire state (docs/triage.md is the
 * mechanism; this is its audit view). */
export function ForkPanel({
  raw,
  wire,
  selection,
}: {
  raw: RawResult;
  wire: WireState;
  selection?: SelectionLines;
}) {
  // The daemon's count when it sent one; the local split is only right when the
  // content on hand is whole.
  const total = selection?.total_lines ?? rustLines(raw.content).length;
  const rows: [string, string][] = [
    ["raw", `${total} ${total === 1 ? "line" : "lines"}`],
  ];
  if (typeof wire === "object" && "Triaged" in wire) {
    rows.push([
      "surfaced",
      `${selection?.kept_lines ?? surfacedLines(wire.Triaged.keep, total)} lines`,
    ]);
    rows.push(["keep", keepLabel(wire.Triaged.keep)]);
  }
  rows.push(["wire", wireLabel(wire)]);
  return (
    <div className="fork-panel">
      <div className="fork-title">what the fork returned</div>
      <div className="fork-grid">
        {rows.map(([k, v]) => (
          <div className="fork-row" key={k}>
            <span className="dim">{k}</span>
            <span className={k === "keep" ? "call-type" : undefined}>{v}</span>
          </div>
        ))}
      </div>
      {typeof wire === "object" &&
        "Triaged" in wire &&
        wire.Triaged.note !== null && (
          <div className="note">
            <span className="note-glyph">✧</span>
            <span className="note-text">{wire.Triaged.note}</span>
          </div>
        )}
    </div>
  );
}

const OUTCOME_LABEL: Record<ToolOutcome, string> = {
  applied: "applied",
  rejected: "rejected",
  harnesserror: "harness error",
};

const formatBytes = (n: number): string =>
  n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KiB`;

/** "What the call cost": the harness's own measurement of the dispatch
 * (docs/tools.md), as distinct from `ForkPanel`, which reports what the fork did
 * to the result afterwards. `duration_ms` excludes the permission cascade, so a
 * call that waited at a gate does not read as a slow call; `result_bytes` is the
 * raw size before triage, which is what makes the pair with the fork panel
 * legible. */
export function TelemetryPanel({ telemetry }: { telemetry: ToolTelemetry }) {
  const rows: [string, string][] = [
    ["outcome", OUTCOME_LABEL[telemetry.outcome] ?? telemetry.outcome],
    ["duration", `${telemetry.duration_ms} ms`],
    ["result", formatBytes(telemetry.result_bytes)],
  ];
  if (telemetry.images !== undefined && telemetry.images > 0) {
    rows.push([
      "images",
      `${telemetry.images} ${telemetry.images === 1 ? "block" : "blocks"}`,
    ]);
  }
  const bash = telemetry.bash;
  if (bash != null) {
    // Absent `exit_status` on a completed foreground command is exit zero — the
    // log omits the field rather than writing "0", so the label is restored here.
    rows.push([
      "exit",
      bash.background ? "backgrounded" : (bash.exit_status ?? "0"),
    ]);
  }
  const edit = telemetry.edit;
  if (edit != null) {
    if (edit.operation != null) rows.push(["operation", edit.operation]);
    if (edit.anchor_bytes != null) {
      rows.push(["anchor", formatBytes(edit.anchor_bytes)]);
    }
    if (edit.content_bytes != null) {
      rows.push(["content", formatBytes(edit.content_bytes)]);
    }
  }
  return (
    <div className="fork-panel">
      <div className="fork-title">what the call cost</div>
      <div className="fork-grid">
        {rows.map(([k, v]) => (
          <div className="fork-row" key={k}>
            <span className="dim">{k}</span>
            <span>{v}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
