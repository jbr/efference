// The inspector (docs/web-client.md): a call-scoped right drawer — "raw is
// one tap away" promoted from a per-result <details> to a standing surface.
// Everything it shows the store already holds, entry-whole; this is a second
// view over the same CallNode the transcript renders, so it re-uses the
// cordon's renderers rather than growing its own.

import { useMemo, useRef, useState } from "react";
import type { Keep, RawResult, WireState } from "../api/types.ts";
import type { CallNode } from "../session/tree.ts";
import { Code } from "./Code.tsx";
import { ResultBody } from "./ToolBlocks.tsx";
import { callView, resultRender, wireLabel } from "../render/parse.ts";

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

type Tab = "result" | "args" | "raw" | "spot" | "file";
const TABS: Tab[] = ["result", "args", "raw", "spot", "file"];

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
function ForkPanel({ raw, wire }: { raw: RawResult; wire: WireState }) {
  const total = raw.content.split("\n").length;
  const rows: [string, string][] = [
    ["raw", `${total} ${total === 1 ? "line" : "lines"}`],
  ];
  if (typeof wire === "object" && "Distilled" in wire) {
    rows.push([
      "surfaced",
      `${surfacedLines(wire.Distilled.keep, total)} lines`,
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

export function Inspector({
  node,
  closing,
  onSlideOut,
  onClose,
}: {
  node: CallNode;
  /** The exit transition is running; `onSlideOut` unmounts when it lands. */
  closing: boolean;
  onSlideOut: () => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<Tab>("result");
  const call = useMemo(() => {
    const issued = node.issued.payload;
    return "Assistant" in issued
      ? (issued.Assistant.calls.find((c) => c.id === node.callId) ?? null)
      : null;
  }, [node.issued, node.callId]);
  const view = useMemo(
    () => (call !== null ? callView(call.name, call.input) : null),
    [call],
  );
  const result =
    node.result !== null && "ToolResult" in node.result.payload
      ? node.result.payload.ToolResult
      : null;
  const rendered = useMemo(
    () =>
      result !== null ? resultRender(node.name, result.raw, result.wire) : null,
    [node.name, result],
  );
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
          <span className="call-tool">{node.name}</span>
          {view?.summary != null && (
            <>
              <span className="call-sep">·</span>
              <span className="call-target">{view.summary}</span>
            </>
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
            // The file tab is the punted full-screen file view
            // (backlog/web/file-view.md): present so the shape is honest,
            // inert until the endpoint exists.
            disabled={t === "file"}
            title={t === "file" ? "file view — not built yet" : undefined}
            onClick={() => setTab(t)}
          >
            {t}
          </button>
        ))}
      </div>
      <div className="inspector-body">
        {tab === "result" &&
          (rendered !== null ? (
            <ResultBody body={rendered.body} />
          ) : (
            <div className="dim">no result yet</div>
          ))}
        {tab === "args" && view !== null && (
          <>
            {view.intent !== null && (
              <div className="intent">{view.intent}</div>
            )}
            {view.body !== null ? (
              <Code text={view.body.text} lang={view.body.lang} />
            ) : (
              <div className="dim">no body — the header says it all</div>
            )}
          </>
        )}
        {tab === "raw" &&
          (result !== null ? (
            <pre>{result.raw.content}</pre>
          ) : (
            <div className="dim">no result yet</div>
          ))}
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
      {result !== null && <ForkPanel raw={result.raw} wire={result.wire} />}
    </div>
  );
}
