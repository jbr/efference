import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Code } from "../Code.tsx";
import type { Process, ProcessRecord } from "../../api/types.ts";
import {
  fetchProcessRecord,
  killProcess,
  patchProcess,
} from "../../api/http.ts";

export function CommandView({
  process,
  processId,
  command,
  output,
  lost,
  running = false,
  completed = false,
}: {
  process?: Process;
  processId?: number;
  command: string;
  output: string;
  lost: number;
  running?: boolean;
  completed?: boolean;
}) {
  const [record, setRecord] = useState<ProcessRecord | null>(null);
  useEffect(() => {
    if (process || processId === undefined) return;
    let active = true;
    void fetchProcessRecord(processId)
      .then((value) => {
        if (active) setRecord(value);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [process, processId]);
  const exit = record?.exit;
  const ended = exit
    ? `Finished · ${typeof exit.outcome === "object" ? `exit ${exit.outcome.code}` : exit.outcome === "signal" ? "terminated by signal" : "exit status unknown"} · ${(exit.duration_ms / 1000).toFixed(1)}s${record?.pid != null ? ` · pid ${record.pid}` : ""}`
    : record && !record.running
      ? "Not running · end not observed"
      : completed
        ? "Finished"
        : running
          ? "Running · foreground"
          : "Process state unavailable";
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
      <pre
        ref={tail}
        onScroll={(e) => {
          const el = e.currentTarget;
          const bottom = el.scrollHeight - el.clientHeight - el.scrollTop <= 1;
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
    </section>
  );
}
