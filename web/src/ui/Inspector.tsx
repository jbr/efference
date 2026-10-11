// The inspector (docs/web-client.md): a call-scoped right drawer — "raw is
// one tap away" promoted from a per-result <details> to a standing surface.
// Everything it shows the store already holds, entry-whole; this is a second
// view over the same CallNode the transcript renders, so it re-uses the
// cordon's renderers rather than growing its own.

import { useEffect, useMemo, useRef, useState } from "react";
import type {
  CallId,
  DistillateLines,
  Keep,
  RawResult,
  ToolOutcome,
  ToolTelemetry,
  WireState,
} from "../api/types.ts";
import type { CallNode } from "../session/tree.ts";
import { assistantView } from "../session/assistant.ts";
import { ArgsTab } from "./ArgsTab.tsx";
import { CallSignature } from "./CallHead.tsx";
import { ResultBody, ResultImages } from "./ToolBlocks.tsx";
import { resultRender, wireLabel } from "../render/parse.ts";
import { callView } from "./signature.ts";
import { layoutSignature } from "./signatureLayout.ts";
import { openablePaths } from "../session/touchedFiles.ts";
import { useWholeResult } from "./wholeResult.ts";
import { rustLines } from "../render/distill.ts";
import { fetchProcessOutput } from "../api/http.ts";

/** Drag-to-resize, persisted. The drawer floats over the content (the
 * content column never moves for it — index.css), so resizing is purely the
 * drawer's own reflow. Clamped so it can neither collapse below usability
 * nor swallow the whole viewport. */
const WIDTH_KEY = "inspector-width";
const clampWidth = (w: number) =>
  Math.min(Math.max(w, 280), Math.max(320, window.innerWidth - 320));

function useDrawerWidth() {
  const [width, setWidth] = useState(() =>
    clampWidth(Number(localStorage.getItem(WIDTH_KEY)) || 396),
  );
  const dragging = useRef(false);
  const grip = {
    onPointerDown: (e: React.PointerEvent<HTMLDivElement>) => {
      dragging.current = true;
      e.currentTarget.setPointerCapture(e.pointerId);
      e.preventDefault();
    },
    onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => {
      if (!dragging.current) return;
      const w = clampWidth(window.innerWidth - e.clientX);
      setWidth(w);
      localStorage.setItem(WIDTH_KEY, String(w));
    },
    onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => {
      dragging.current = false;
      e.currentTarget.releasePointerCapture(e.pointerId);
    },
  };
  return { width, grip };
}

// No `file` tab: the drawer is call-scoped, so a tab here reads as "the file
// at the time of this call", while a file read from disk is the file *now* —
// and serving the historical reading would mean snapshotting every file at
// every edit. The division is that the inspector shows what a call did (from
// the log, which holds it) and the file view shows what a file is
// (backlog/web/file-view/README.md); the viewer opens from `open ⤢`, labelled
// as current.
type Tab = "result" | "args" | "raw" | "spot";
const TABS: Tab[] = ["result", "args", "raw", "spot"];

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
 * what survived, the keep, the wire state (docs/redaction-fork.md is the
 * mechanism; this is its audit view). */
function ForkPanel({
  raw,
  wire,
  distillate,
}: {
  raw: RawResult;
  wire: WireState;
  distillate?: DistillateLines;
}) {
  // The daemon's count when it sent one; the local split is only right when the
  // content on hand is whole.
  const total = distillate?.total_lines ?? rustLines(raw.content).length;
  const rows: [string, string][] = [
    ["raw", `${total} ${total === 1 ? "line" : "lines"}`],
  ];
  if (typeof wire === "object" && "Distilled" in wire) {
    rows.push([
      "surfaced",
      `${distillate?.kept_lines ?? surfacedLines(wire.Distilled.keep, total)} lines`,
    ]);
    rows.push(["keep", keepLabel(wire.Distilled.keep)]);
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
        "Distilled" in wire &&
        wire.Distilled.note !== null && (
          <div className="note">
            <span className="note-glyph">✧</span>
            <span className="note-text">{wire.Distilled.note}</span>
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
 * raw size before distillation, which is what makes the pair with the fork panel
 * legible. */
function TelemetryPanel({ telemetry }: { telemetry: ToolTelemetry }) {
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
  if (bash !== undefined) {
    // Absent `exit_status` on a completed foreground command is exit zero — the
    // log omits the field rather than writing "0", so the label is restored here.
    rows.push([
      "exit",
      bash.background ? "backgrounded" : (bash.exit_status ?? "0"),
    ]);
  }
  const edit = telemetry.edit;
  if (edit !== undefined) {
    if (edit.operation !== undefined) rows.push(["operation", edit.operation]);
    if (edit.anchor_bytes !== undefined) {
      rows.push(["anchor", formatBytes(edit.anchor_bytes)]);
    }
    if (edit.content_bytes !== undefined) {
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

/** What the command itself printed, for the two cases where the call's
 * `ToolResult` is not it (docs/supervisor.md):
 *
 * - **running**: no result has committed yet, and the live tail is already in the
 *   store as `streaming.toolOutput[callId]` — the transcript's inline peek renders
 *   it, and this reads the same buffer.
 * - **backgrounded**: the committed `ToolResult` is the *handoff* result ("moved to
 *   the background as p#3"), not the output. The output lives in the supervisor's
 *   retained capture, reached through the call↔process mapping the daemon carries
 *   on each process row.
 *
 * A capture that hit the retention cap is genuinely incomplete, and says so: `lost`
 * is rendered as a count of dropped bytes rather than letting a truncated capture
 * present as the whole output. */
interface OutputRead {
  output: string;
  lost: number;
}

/** The supervisor's capture for a process, if we can reach it.
 *
 * While the process is live the store already holds its tail (seeded cold from the
 * same route by `ProcessSeeder`, then extended by `process_output` deltas), so that
 * buffer is used as-is. Once it has exited it leaves the `processes` topic and the
 * deltas stop, and there is nothing buffered — then this reads the retained capture
 * directly, which the daemon keeps serving after the exit. */
function useRetainedOutput(
  processId: number | undefined,
  buffered: string | undefined,
  bufferedLost: number | undefined,
): OutputRead | null {
  // Keyed by the id it was read for, so a drawer that switches to another call
  // discards the previous capture at render time rather than clearing it from
  // inside the effect (a synchronous setState there cascades renders).
  const [fetched, setFetched] = useState<{
    id: number;
    read: OutputRead;
  } | null>(null);
  // The dependency is *whether* a live buffer exists, not its contents: the
  // buffer grows with every delta, and depending on the string would re-read
  // the retained capture per chunk.
  const buffering = buffered !== undefined;
  useEffect(() => {
    if (processId === undefined || buffering) return;
    let live = true;
    void fetchProcessOutput(processId)
      .then((read) => {
        if (live) setFetched({ id: processId, read });
      })
      // A capture the daemon no longer has (restarted, never registered) is a
      // drawer with no output section, not an error to surface here.
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [processId, buffering]);
  if (processId === undefined) return null;
  if (buffered !== undefined)
    return { output: buffered, lost: bufferedLost ?? 0 };
  return fetched !== null && fetched.id === processId ? fetched.read : null;
}

function OutputSection({ label, read }: { label: string; read: OutputRead }) {
  return (
    <div className="inspector-output">
      <div className="dim">{label}</div>
      {read.lost > 0 && (
        <div className="note">
          <span className="note-glyph">✧</span>
          <span className="note-text">
            output incomplete; {read.lost} bytes dropped at the retention cap
          </span>
        </div>
      )}
      <pre>{read.output === "" ? "(no output yet)" : read.output}</pre>
    </div>
  );
}

export function Inspector({
  node,
  closing,
  onSlideOut,
  onClose,
  onOpenFile,
  liveOutput,
  processId,
  processOutput,
  processLost,
}: {
  node: CallNode;
  /** The exit transition is running; `onSlideOut` unmounts when it lands. */
  closing: boolean;
  onSlideOut: () => void;
  onClose: () => void;
  onOpenFile: (path: string, callId: CallId) => void;
  /** `streaming.toolOutput[callId]`: the live tail of a call still running. */
  liveOutput: string | undefined;
  /** The process this call spawned, from the call↔process mapping. */
  processId: number | undefined;
  /** `streaming.processOutput[processId]`, while the process is still live. */
  processOutput: string | undefined;
  processLost: number | undefined;
}) {
  const [tab, setTab] = useState<Tab>("result");
  const call = useMemo(() => {
    const issued = node.issued.payload;
    return "Assistant" in issued
      ? (assistantView(issued.Assistant).calls.find(
          (c) => c.id === node.callId,
        ) ?? null)
      : null;
  }, [node.issued, node.callId]);
  const view = useMemo(
    () => (call !== null ? callView(call.name, call.input) : null),
    [call],
  );
  const entry =
    node.result !== null && "ToolResult" in node.result.payload
      ? node.result
      : null;
  const payload =
    entry !== null && "ToolResult" in entry.payload
      ? entry.payload.ToolResult
      : null;
  // Inspecting *is* the request for the whole thing, so a truncated result
  // (docs/control-api.md) dereferences itself here rather than waiting for a
  // second click — the transcript is where the on-demand affordance lives.
  const whole = useWholeResult(entry?.seq ?? null, payload?.raw ?? null);
  useEffect(() => {
    if (whole.elided !== undefined) whole.fetchWhole();
  }, [whole.elided, whole.fetchWhole, whole]);
  // Memoized so the identity is stable across renders: the render below keys
  // its own memo on it.
  const wholeRaw = whole.raw;
  const result = useMemo(
    () =>
      payload !== null && wholeRaw !== null
        ? { ...payload, raw: wholeRaw }
        : null,
    [payload, wholeRaw],
  );
  const rendered = useMemo(
    () =>
      result !== null
        ? resultRender(node.name, result.raw, result.wire, result.distillate)
        : null,
    [node.name, result],
  );
  const retained = useRetainedOutput(processId, processOutput, processLost);
  // The live tail wins where both exist: for a foreground call it is the buffer
  // that is still growing, while the capture read is a point-in-time snapshot.
  const output: { label: string; read: OutputRead } | null =
    liveOutput !== undefined
      ? {
          label: "live output · running",
          read: { output: liveOutput, lost: 0 },
        }
      : retained !== null
        ? { label: `p#${processId} output`, read: retained }
        : null;
  const { width, grip } = useDrawerWidth();
  return (
    <div
      className={closing ? "inspector closing" : "inspector"}
      style={{ width }}
      onTransitionEnd={(e) => {
        // Keyed to the transform: only the slide's own end means done, not
        // any other property a future style might transition.
        if (
          closing &&
          e.target === e.currentTarget &&
          e.propertyName === "transform"
        )
          onSlideOut();
      }}
    >
      <div className="inspector-grip" {...grip} />
      <div className="inspector-head">
        <span className="inspector-title">inspecting</span>
        <span className="call-head">
          {/* One line in the head, wrapping as text: the inspector's
              body is where the arguments are read at length. */}
          {view !== null && (
            <CallSignature layout={layoutSignature(view, null)} />
          )}
        </span>
        <span className="spacer" />
        <button className="linkish" onClick={onClose}>
          ✕
        </button>
      </div>
      <div className="inspector-tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={t === tab ? "inspector-tab open" : "inspector-tab"}
            onClick={() => setTab(t)}
          >
            {t}
          </button>
        ))}
      </div>
      <div className="inspector-body">
        {tab === "result" && (
          <>
            {rendered !== null ? (
              <ResultBody body={rendered.body} />
            ) : (
              output === null && <div className="dim">no result yet</div>
            )}
            {/* The image is part of the result, and its absence is what made
                this drawer useless for a picture-returning call. It opens the
                same lightbox the transcript does (lightbox.ts). */}
            {result !== null && (
              <ResultImages
                seq={entry?.seq ?? 0}
                images={result.raw.images ?? []}
              />
            )}
            {output !== null && (
              <OutputSection label={output.label} read={output.read} />
            )}
          </>
        )}
        {tab === "args" && call !== null && view !== null && (
          <ArgsTab name={call.name} input={call.input} intent={view.intent} />
        )}
        {tab === "raw" && (
          <>
            {result !== null ? (
              <pre>{result.raw.content}</pre>
            ) : (
              output === null && <div className="dim">no result yet</div>
            )}
            {output !== null && (
              <OutputSection label={output.label} read={output.read} />
            )}
          </>
        )}
        {tab === "spot" &&
          (node.spot !== null ? (
            <div className="inspector-spot">
              <div>
                <span className="spot-label">spot · {node.spot.stance}</span>
                <span className="dim"> · {node.spot.origin}</span>
              </div>
              <div className="spot-reason">{node.spot.reason}</div>
              {node.spot.probed_prefix !== undefined && (
                <div className="dim">probed: {node.spot.probed_prefix}</div>
              )}
            </div>
          ) : (
            <div className="dim">no verdict logged</div>
          ))}
      </div>
      {call !== null && (
        // In the footer, and labelled current: this drawer is the historical
        // record of what the call did; the file on disk is *now*, which is
        // why there is no `file` tab (backlog/web/file-view/README.md).
        <InspectorOpen
          name={call.name}
          input={call.input}
          callId={node.callId}
          onOpenFile={onOpenFile}
        />
      )}
      {node.telemetry !== null && <TelemetryPanel telemetry={node.telemetry} />}
      {result !== null && (
        <ForkPanel
          raw={result.raw}
          wire={result.wire}
          distillate={result.distillate}
        />
      )}
    </div>
  );
}

function InspectorOpen({
  name,
  input,
  callId,
  onOpenFile,
}: {
  name: string;
  input: unknown;
  /** The call whose edit the viewer should land on (session/editAnchor.ts). */
  callId: CallId;
  onOpenFile: (path: string, callId: CallId) => void;
}) {
  const paths = useMemo(() => openablePaths(name, input), [name, input]);
  if (paths.length === 0) return null;
  return (
    <div className="inspector-open">
      {paths.map((path) => (
        <button
          key={path}
          className="linkish open-file"
          title={`view ${path} as it is now on disk`}
          onClick={() => onOpenFile(path, callId)}
        >
          open {paths.length === 1 ? "current file" : path} ⤢
        </button>
      ))}
    </div>
  );
}
