import { useEffect, useMemo, useState } from "react";
import type { CallId } from "../../api/types.ts";
import type { LogEntry } from "../../api/entry.ts";
import { fetchCallSeq, fetchLogRange } from "../../api/http.ts";
import { buildTree, type CallNode } from "../../session/tree.ts";

export function useHistoricalCall(
  id: CallId | null,
  loaded: CallNode | null,
  entries: LogEntry[],
  base: string,
) {
  const [window, setWindow] = useState<{
    id: CallId;
    entries: LogEntry[];
  } | null>(null);
  const [failure, setFailure] = useState<{
    id: CallId;
    message: string;
  } | null>(null);
  const missing = id !== null && loaded === null;
  useEffect(() => {
    if (!missing || id === null) return;
    let active = true;
    const end = entries.at(-1)?.seq ?? 0;
    void fetchCallSeq(id, base)
      .then(({ seq }) => fetchLogRange(seq, end + 1, base))
      .then((rows) => {
        if (active) setWindow({ id, entries: rows });
      })
      .catch((e) => {
        if (active) setFailure({ id, message: String(e) });
      });
    return () => {
      active = false;
    };
    // Capture the initial end; subsequent live entries merge below rather than restarting this read.
  }, [id, missing, base]); // eslint-disable-line react-hooks/exhaustive-deps
  const historical = useMemo(() => {
    if (id === null || window?.id !== id) return null;
    const rows = new Map(window.entries.map((e) => [e.seq, e]));
    for (const entry of entries) rows.set(entry.seq, entry);
    for (const node of buildTree(
      [...rows.values()].sort((a, b) => a.seq - b.seq),
    )) {
      if (node.kind === "batch") {
        const call = node.calls.find((c) => c.callId === id);
        if (call) return call;
      }
    }
    return null;
  }, [id, window, entries]);
  return {
    node: loaded ?? historical,
    error: failure?.id === id ? failure.message : null,
  };
}
