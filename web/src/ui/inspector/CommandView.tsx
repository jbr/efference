import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Code } from "../Code.tsx";
import { ResultRanges } from "../ResultRanges.tsx";
import type { ResultRender } from "../../render/parse.ts";
import type {
  Process,
  ProcessRecord,
  RawResult,
  ToolTelemetry,
  WireState,
} from "../../api/types.ts";
import {
  fetchProcessRecord,
  killProcess,
  patchProcess,
} from "../../api/http.ts";

/** A command's inspection (docs/web-client.md): its status, and whichever of
 * three bodies holds what it printed — the live tail while it runs, the
 * process monitor's capture once it was backgrounded, or, for a command that
 * finished in the foreground, its committed result with the triage verdict's
 * kept lines and expandable gaps. */
export function CommandView({
  process,
  processId,
  command,
  output,
  lost,
  running = false,
  handedOff = false,
  result,
  telemetry,
}: {
  process?: Process;
  /** The process to ask the process monitor about; absent when the committed
   * result is the output, since a finished foreground command leaves no
   * record there. */
  processId?: number;
  command: string;
  /** The live tail or the retained capture. */
  output: string;
  lost: number;
  running?: boolean;
  /** The committed result is the handoff to the background, not the output. */
  handedOff?: boolean;
  /** The committed result, when it is the command's output rather than a
   * handoff. */
  result?: {
    tool: string;
    raw: RawResult;
    wire: WireState;
    rendered: ResultRender;
  };
  telemetry?: ToolTelemetry | null;
}) {
  // Whether the fetch has settled is state of its own: before it has, there
  // is nothing to say "unavailable" about.
  const [record, setRecord] = useState<ProcessRecord | null | "failed">(null);
  useEffect(() => {
    if (process || processId === undefined) return;
    let active = true;
    void fetchProcessRecord(processId)
      .then((value) => {
        if (active) setRecord(value);
      })
      .catch(() => {
        if (active) setRecord("failed");
      });
    return () => {
      active = false;
    };
  }, [process, processId]);
  const read = record === "failed" ? null : record;
  const exit = read?.exit;
  const ended = exit
    ? `Finished · ${typeof exit.outcome === "object" ? `exit ${exit.outcome.code}` : exit.outcome === "signal" ? "terminated by signal" : "exit status unknown"} · ${duration(exit.duration_ms)}${read?.pid != null ? ` · pid ${read.pid}` : ""}`
    : read && !read.running
      ? "Not running · end not observed"
      : result
        ? finishedLine(telemetry)
        : running
          ? "Running · foreground"
          : processId === undefined
            ? handedOff
              ? "Backgrounded · process state unavailable"
              : "No result yet"
            : record === "failed"
              ? "Process state unavailable"
              : "Reading process state…";
  const tail = useRef<HTMLPreElement>(null);
  const [following, setFollowing] = useState(true);
  const followingRef = useRef(true);
  useLayoutEffect(() => {
    const el = tail.current;
    if (el && followingRef.current) el.scrollTop = el.scrollHeight;
  }, [output]);
  useEffect(() => {
    const el = tail.current;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      if (followingRef.current) el.scrollTop = el.scrollHeight;
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const [now, setNow] = useState(() => Date.now());
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  async function act(request: () => Promise<unknown>) {
    setPending(true);
    try {
      await request();
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setPending(false);
    }
  }
  return (
    <section className="command-inspection">
      <p className="dim">
        {process
          ? `Running · ${process.background ? "background" : "foreground"} · ${Math.max(0, Math.floor((now - process.spawned_at) / 1000))}s · pid ${process.pid}`
          : ended}
      </p>
      {process && (
        <div className="command-actions">
          {!process.background && (
            <button
              disabled={pending}
              onClick={() =>
                void act(() => patchProcess(process.id, { background: true }))
              }
            >
              Background
            </button>
          )}
          <button
            disabled={pending}
            onClick={() =>
              void act(() =>
                patchProcess(process.id, { muted: !process.muted }),
              )
            }
          >
            {process.muted ? "Unmute" : "Mute"}
          </button>
          <button
            disabled={pending}
            onClick={() => void act(() => killProcess(process.id))}
          >
            Kill
          </button>
        </div>
      )}
      {error && <div className="error">{error}</div>}
      {lost > 0 && (
        <p className="dim">Output incomplete: {lost} earlier bytes dropped.</p>
      )}
      <h4>Command</h4>
      <Code text={command} lang="bash" />
      <div className="command-output-heading">
        <h4>Output</h4>
        {result && resultLabel(result.rendered) && (
          <span className="dim">{resultLabel(result.rendered)}</span>
        )}
        {(process || running) &&
          (following ? (
            <span className="dim">Following</span>
          ) : (
            <button
              onClick={() => {
                followingRef.current = true;
                setFollowing(true);
                if (tail.current)
                  tail.current.scrollTop = tail.current.scrollHeight;
              }}
            >
              Follow output
            </button>
          ))}
      </div>
      {result ? (
        <>
          {result.rendered.note && (
            <p className="dim">{result.rendered.note}</p>
          )}
          {result.raw.content === "" ? (
            <p className="dim">(no output)</p>
          ) : (
            <ResultRanges
              tool={result.tool}
              raw={result.raw}
              wire={result.wire}
              lang={null}
            />
          )}
        </>
      ) : (
        <pre
          ref={tail}
          onScroll={(e) => {
            const el = e.currentTarget;
            const bottom =
              el.scrollHeight - el.clientHeight - el.scrollTop <= 1;
            followingRef.current = bottom;
            setFollowing(bottom);
          }}
          className="command-output"
          style={{
            overflow: "auto",
            maxHeight: "50dvh",
            whiteSpace: "pre-wrap",
            overflowWrap: "anywhere",
          }}
        >
          {output || "(no output yet)"}
        </pre>
      )}
    </section>
  );
}

/** The status of a command whose result committed in the foreground. The
 * telemetry row lands just after the result, so for a moment there is only
 * the word. An absent `exit_status` on a completed foreground command is exit
 * zero (docs/tools.md). */
function finishedLine(telemetry: ToolTelemetry | null | undefined): string {
  if (!telemetry) return "Finished";
  const status = telemetry.bash?.exit_status ?? "0";
  const exit = status === "signal" ? "terminated by signal" : `exit ${status}`;
  return `Finished · ${exit} · ${duration(telemetry.duration_ms)}`;
}

/** Milliseconds under a second, so a quick command does not read "0.0s". */
const duration = (ms: number): string =>
  ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)}s`;

/** The transcript result head's words, less the bare "raw" — in a drawer
 * that only ever shows the command's own output, that word says nothing. */
function resultLabel(rendered: ResultRender): string | null {
  const parts = [
    rendered.stateWord === "raw" ? null : rendered.stateWord,
    rendered.sizeLabel,
    rendered.elisionLabel,
    rendered.isError ? "error" : null,
  ].filter((part) => part !== null);
  return parts.length > 0 ? parts.join(", ") : null;
}
