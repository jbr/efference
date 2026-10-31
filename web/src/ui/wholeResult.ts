// Dereferencing a truncated tool result (docs/control-api.md).
//
// The window read and the control stream carry an oversized result's `content`
// cut at 16 KiB, tagged with `content_elided`. `GET /v1/log/{seq}` serves the
// same entry whole — the same shape as an image reference, one route over — so
// a client that wants the rest asks for the entry rather than for a new kind of
// payload. ETag-validated, so a second look is a 304.

import { useCallback, useState } from "react";
import { fetchLogEntry } from "../api/http.ts";
import type { LogEntry, RawResult, Seq } from "../api/entry.ts";

/** The `RawResult` of a `ToolResult` entry, or null for any other payload. */
export function resultOf(entry: LogEntry): RawResult | null {
  const payload = entry.payload;
  if (typeof payload !== "object" || !("ToolResult" in payload)) return null;
  return payload.ToolResult.raw;
}

export type FetchState = "idle" | "fetching" | "failed";

/**
 * The whole result, once asked for.
 *
 * Returns the truncated copy until a fetch lands, then the whole one — so a
 * caller renders the same way either way and only the marker changes. `elided`
 * is the tag while it still applies, which is what an affordance keys off:
 * it goes away when the content on screen is complete.
 */
export function useWholeResult(seq: Seq | null, raw: RawResult | null) {
  const [whole, setWhole] = useState<RawResult | null>(null);
  const [state, setState] = useState<FetchState>("idle");

  const fetchWhole = useCallback(() => {
    if (seq === null || whole !== null || state === "fetching") return;
    setState("fetching");
    fetchLogEntry(seq).then(
      (entry) => {
        const result = resultOf(entry);
        // A failure is a value, not a throw: the truncated content stays on
        // screen and the marker says the rest did not arrive.
        if (result === null || result.content_elided != null) {
          setState("failed");
          return;
        }
        setWhole(result);
        setState("idle");
      },
      () => setState("failed"),
    );
  }, [seq, whole, state]);

  return {
    raw: whole ?? raw,
    elided: whole === null ? (raw?.content_elided ?? undefined) : undefined,
    state,
    fetchWhole,
  };
}
