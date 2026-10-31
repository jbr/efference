import { anchorFromResult, type DiffAnchor } from "../session/editAnchor.ts";
import { FileOperation } from "./inspector/FileOperation.tsx";
import { fileOperation } from "./inspector/fileCapture.ts";
import { useDrawerWidth } from "./inspector/useDrawerWidth.ts";
import { isHandoff } from "./inspector/handoff.ts";
import { Disclosure } from "./Disclosure.tsx";
import "./inspector/Layout.css";
// The inspector (docs/web-client.md): a call-scoped right drawer — "raw is
// one tap away" promoted from a per-result <details> to a standing surface.
// Everything it shows the store already holds, entry-whole; this is a second
// view over the same CallNode the transcript renders, so it re-uses the
// cordon's renderers rather than growing its own.

import { useEffect, useMemo, useState } from "react";
import type { CallId, ProcessState } from "../api/types.ts";
import type { CallNode } from "../session/tree.ts";
import { assistantView } from "../session/assistant.ts";
import { CommandView } from "./inspector/CommandView.tsx";
import { ForkPanel, TelemetryPanel } from "./inspector/DetailPanels.tsx";
import { ArgsTab } from "./ArgsTab.tsx";
import { CallSignature } from "./CallHead.tsx";
import { ResultBody, ResultImages } from "./ToolBlocks.tsx";
import { resultRender } from "../render/parse.ts";
import { callView } from "./signature.ts";
import { layoutSignature } from "./signatureLayout.ts";
import { openablePaths } from "../session/touchedFiles.ts";
import { useWholeResult } from "./wholeResult.ts";
import { fetchProcessOutput } from "../api/http.ts";

// Generic tools retain result/args/raw/spot views; file operations use captured
// content and navigate to explicitly current-file context (docs/web-client.md).
type Tab = "result" | "args" | "raw" | "spot";
const TABS: Tab[] = ["result", "args", "raw", "spot"];

/** What the command itself printed, for the two cases where the call's
 * `ToolResult` is not it (docs/process-monitor.md):
 *
 * - **running**: no result has committed yet, and the live tail is already in the
 *   store as `streaming.toolOutput[callId]` — the transcript's inline peek renders
 *   it, and this reads the same buffer.
 * - **backgrounded**: the committed `ToolResult` is the *handoff* result ("moved to
 *   the background as p#3"), not the output. The output lives in the process monitor's
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

/** The process monitor's capture for a process, if we can reach it.
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
  process,
  closing,
  onSlideOut,
  onClose,
  onOpenFile,
  hidden = false,
  liveOutput,
  processId: suppliedProcessId,
  processOutput,
  processLost,
}: {
  node: CallNode;
  hidden?: boolean;
  process?: ProcessState;
  /** The exit transition is running; `onSlideOut` unmounts when it lands. */
  closing: boolean;
  onSlideOut: () => void;
  onClose: () => void;
  onOpenFile: (
    path: string,
    callId: CallId,
    anchor?: DiffAnchor | null,
  ) => void;
  /** `streaming.toolOutput[callId]`: the live tail of a call still running. */
  liveOutput: string | undefined;
  /** The process this call spawned, from the call↔process mapping. */
  processId: number | undefined;
  /** `streaming.processOutput[processId]`, while the process is still live. */
  processOutput: string | undefined;
  processLost: number | undefined;
}) {
  const recordedHandle = node.telemetry?.bash?.handle;
  const recordedId = recordedHandle?.match(/^p#(\d+)$/)?.[1];
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
  const backgroundCapable =
    node.telemetry?.bash?.background === true ||
    view?.shell?.background === true ||
    view?.shell?.backgroundAfter != null;
  const processId =
    suppliedProcessId ??
    (backgroundCapable && recordedId !== undefined
      ? Number(recordedId)
      : undefined);
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
    if (whole.elided !== undefined && whole.state === "idle")
      whole.fetchWhole();
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
        ? resultRender(node.name, result.raw, result.wire, result.selection)
        : null,
    [node.name, result],
  );
  // A foreground command's committed result *is* its output, so the handoff
  // test reads the result rather than the process mapping: every bash call
  // spawns into the one process namespace (docs/process-monitor.md), so having a
  // process id says nothing about where the output lives. The mapping stands in
  // for the handle until the telemetry row that records it lands.
  const handle =
    recordedHandle ?? (processId !== undefined ? `p#${processId}` : undefined);
  const backgroundHandoff =
    isHandoff(result?.raw.content ?? "", handle) ||
    node.telemetry?.bash?.background === true ||
    view?.shell?.background === true;
  const commandResult = !backgroundHandoff ? result : null;
  // The process monitor holds a capture only while the result is not the
  // output: a foreground command is dropped from the registry when its call
  // completes, so asking after it answers 404 by design.
  const captureId = commandResult === null ? processId : undefined;
  const retained = useRetainedOutput(captureId, processOutput, processLost);
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
  const fileInspection = fileOperation(node.name);
  const filePaths = call ? openablePaths(call.name, call.input) : [];
  const commandInspection =
    view?.shell != null || processId !== undefined || process !== undefined;
  const command =
    process?.command ??
    (call?.input && typeof call.input === "object" && "command" in call.input
      ? String(call.input.command)
      : node.name);
  const { width, grip } = useDrawerWidth();
  return (
    <div
      className={`inspector${closing ? " closing" : ""}${fileInspection ? " is-file-operation" : ""}`}
      style={{ width, display: hidden ? "none" : undefined }}
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
        {fileInspection ? (
          <strong>{filePaths.length === 1 ? filePaths[0] : node.name}</strong>
        ) : commandInspection ? (
          <strong>
            {command.trim().split(/\s+/)[0]}
            {handle !== undefined && ` · ${handle}`}
          </strong>
        ) : (
          <span className="inspector-title">inspecting</span>
        )}
        <span className="call-head">
          {/* One line in the head, wrapping as text: the inspector's
              body is where the arguments are read at length. */}
          {!commandInspection && !fileInspection && view !== null && (
            <CallSignature layout={layoutSignature(view, null)} />
          )}
        </span>
        <span className="spacer" />
        <button
          className="linkish"
          aria-label="Close inspector"
          onClick={onClose}
        >
          ✕
        </button>
      </div>
      {!commandInspection && !fileInspection && (
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
      )}
      <div className="inspector-body">
        {whole.state === "failed" && (
          <p role="alert">
            Could not load the rest of this result.{" "}
            <button onClick={whole.fetchWhole}>Retry</button>
          </p>
        )}
        {fileInspection && call && (
          <FileOperation
            name={call.name}
            input={call.input}
            raw={result?.raw ?? null}
            wire={result?.wire ?? "Pending"}
            onCurrent={(path) =>
              onOpenFile(
                path,
                node.callId,
                result !== null && !result.raw.is_error
                  ? anchorFromResult(result.raw.content)
                  : null,
              )
            }
          />
        )}
        {!fileInspection && (commandInspection || tab === "result") && (
          <>
            {commandInspection ? (
              <CommandView
                key={node.callId}
                process={process}
                processId={captureId}
                command={command}
                running={liveOutput !== undefined && result === null}
                handedOff={result !== null && backgroundHandoff}
                result={
                  commandResult !== null && rendered !== null
                    ? { ...commandResult, tool: node.name, rendered }
                    : undefined
                }
                telemetry={node.telemetry}
                output={output?.read.output ?? ""}
                lost={output?.read.lost ?? 0}
              />
            ) : rendered !== null ? (
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
            {output !== null &&
              processId === undefined &&
              process === undefined && (
                <OutputSection label={output.label} read={output.read} />
              )}
          </>
        )}
        {!commandInspection &&
          !fileInspection &&
          tab === "args" &&
          call !== null &&
          view !== null && (
            <ArgsTab name={call.name} input={call.input} intent={view.intent} />
          )}
        {!commandInspection && !fileInspection && tab === "raw" && (
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
        {!commandInspection &&
          !fileInspection &&
          tab === "spot" &&
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
      {call !== null && !fileInspection && (
        // Generic tool calls can still open explicitly current-file context.
        <InspectorOpen
          name={call.name}
          input={call.input}
          callId={node.callId}
          onOpenFile={onOpenFile}
        />
      )}
      <Disclosure className="inspector-details" label="Details">
        {fileInspection && call && view && (
          <ArgsTab name={call.name} input={call.input} intent={view.intent} />
        )}
        {/* A finished command's result is already the drawer's body; the
            raw disclosure is for a result that is not the output (a
            handoff) and for file operations, whose body is a capture. */}
        {(fileInspection || (commandInspection && commandResult === null)) &&
          result !== null && (
            <Disclosure label="Raw tool result">
              <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
                {result.raw.content}
              </pre>
            </Disclosure>
          )}
        {(commandInspection || fileInspection) && node.spot !== null && (
          <Disclosure label="Permission verdict">
            <p>
              {node.spot.stance} · {node.spot.origin}
            </p>
            <p>{node.spot.reason}</p>
          </Disclosure>
        )}
        {node.telemetry !== null && (
          <TelemetryPanel telemetry={node.telemetry} />
        )}
        {result !== null && (
          <ForkPanel
            raw={result.raw}
            wire={result.wire}
            selection={result.selection}
          />
        )}
      </Disclosure>
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
  onOpenFile: (
    path: string,
    callId: CallId,
    anchor?: DiffAnchor | null,
  ) => void;
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
