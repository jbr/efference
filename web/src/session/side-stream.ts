import type {
  CallId,
  CurrentOperations,
  LogEntry,
  OutputData,
  RequestStage,
} from "../api/types.ts";

export interface SideText {
  text: string;
  thinking: string;
}

interface RequestTail {
  request: number | null;
  phase: "open" | "retrying" | "closed";
}

export interface SideStreaming {
  compaction: SideText | null;
  consolidation: (SideText & { from: number; upto: number }) | null;
  triage: (SideText & { callIds: CallId[] })[];
  /** Side output has no request id. Durable settlement and request lifecycle
   * fence off queued deltas after their authoritative entries (docs/control-api.md).
   * Unlike the main response drain, these fences survive unrelated log entries. */
  sideRequests: {
    compaction: RequestTail;
    consolidation: RequestTail;
    triage: RequestTail;
    settledCalls: CallId[];
    idleSettled: boolean;
  };
}

export const emptySideStreaming: SideStreaming = {
  compaction: null,
  consolidation: null,
  triage: [],
  sideRequests: {
    compaction: { request: null, phase: "open" },
    consolidation: { request: null, phase: "closed" },
    triage: { request: null, phase: "open" },
    settledCalls: [],
    idleSettled: false,
  },
};

const cap = (text: string) => text.slice(-256 * 1024);

export function closeSide<T extends SideStreaming>(
  s: T,
  kind: "compaction" | "triage" | "consolidation",
  clear = true,
): T {
  return {
    ...s,
    [kind]: clear ? (kind === "triage" ? [] : null) : s[kind],
    sideRequests: {
      ...s.sideRequests,
      [kind]: { ...s.sideRequests[kind], phase: "closed" },
    },
  };
}

/** Checking telemetry supplies the consolidation span before sent. Reset request
 * text without discarding that anchor, including between retry attempts. */
function resetText(
  s: SideStreaming,
  kind: "compaction" | "triage" | "consolidation",
) {
  if (kind === "triage") return [];
  if (kind === "consolidation" && s.consolidation !== null)
    return { ...s.consolidation, text: "", thinking: "" };
  return null;
}

function stage<T extends SideStreaming>(s: T, event: RequestStage): T {
  const kind = event.origin;
  if (kind !== "compaction" && kind !== "triage" && kind !== "consolidation")
    return s;
  const previous = s.sideRequests[kind];
  if (event.stage === "sent") {
    if (previous.request !== null && event.request <= previous.request)
      return s;
    return {
      ...s,
      [kind]: resetText(s, kind),
      sideRequests: {
        ...s.sideRequests,
        [kind]: { request: event.request, phase: "open" },
        ...(kind === "triage" ? { settledCalls: [], idleSettled: false } : {}),
      },
    };
  }
  if (previous.request !== event.request || previous.phase === "closed")
    return s;
  if (
    event.stage === "ended" ||
    (typeof event.stage === "object" && "failed" in event.stage)
  ) {
    return closeSide(s, kind, event.stage !== "ended");
  }
  if (typeof event.stage === "object" && "attempt_failed" in event.stage) {
    return {
      ...s,
      [kind]: resetText(s, kind),
      sideRequests: {
        ...s.sideRequests,
        [kind]: { ...previous, phase: "retrying" },
      },
    };
  }
  if (event.stage === "first_byte" && previous.phase === "retrying") {
    return {
      ...s,
      sideRequests: {
        ...s.sideRequests,
        [kind]: { ...previous, phase: "open" },
      },
    };
  }
  return s;
}

/** State may seed a connection midway through a request. Only a new request id
 * reopens a closed tail: a repeated state snapshot cannot undo a verdict. */
export function sideOperations<T extends SideStreaming>(
  s: T,
  operations: CurrentOperations | null,
): T {
  for (const kind of ["compaction", "triage", "consolidation"] as const) {
    const requests =
      operations?.requests.filter((request) => request.origin === kind) ?? [];
    for (const request of requests) {
      s = stage(s, { ...request, stage: "sent" });
      if (request.stage === "retrying") {
        s = stage(s, {
          ...request,
          stage: {
            attempt_failed: {
              attempt: request.attempt ?? 1,
              status: request.status ?? 0,
            },
          },
        });
      } else if (request.stage === "streaming") {
        s = stage(s, { ...request, stage: "first_byte" });
      }
    }
    if (
      requests.length === 0 &&
      (kind !== "consolidation" ||
        s.sideRequests.consolidation.request !== null)
    )
      s = closeSide(s, kind, false);
  }
  return s;
}

export function sideLog<T extends SideStreaming>(
  s: T,
  entry: LogEntry,
  entries: LogEntry[],
): T {
  const payload = entry.payload;
  if ("Resumed" in payload)
    return closeSide(
      closeSide(closeSide(s, "compaction"), "triage"),
      "consolidation",
    );
  if ("Summary" in payload && payload.Summary.made_by !== undefined)
    return closeSide(s, "compaction");
  if (
    "Summary" in payload &&
    payload.Summary.made_by === undefined &&
    s.consolidation?.from === payload.Summary.from &&
    s.consolidation?.upto === payload.Summary.upto
  )
    return closeSide(s, "consolidation");
  if ("Telemetry" in payload) {
    if ("Consolidation" in payload.Telemetry) {
      const check = payload.Telemetry.Consolidation;
      if (check.outcome !== "checking") return closeSide(s, "consolidation");
      return {
        ...s,
        consolidation: {
          from: check.from,
          upto: check.upto,
          text: "",
          thinking: "",
        },
        sideRequests: {
          ...s.sideRequests,
          consolidation: { request: null, phase: "open" },
        },
      };
    }
    if ("RequestStage" in payload.Telemetry)
      return stage(s, payload.Telemetry.RequestStage);
    if ("CompactionFailed" in payload.Telemetry)
      return closeSide(s, "compaction");
  }
  let result: number | null;
  if ("Triage" in payload) result = payload.Triage.for_result;
  else if ("ResultVerdict" in payload)
    result = payload.ResultVerdict.for_result;
  else return s;
  const callId =
    result === null
      ? null
      : entries.flatMap((entry) =>
          entry.seq === result && "ToolResult" in entry.payload
            ? [entry.payload.ToolResult.call_id]
            : [],
        )[0];
  if (callId === undefined) return s;
  return {
    ...s,
    triage: s.triage.filter((tail) =>
      callId === null
        ? tail.callIds.length !== 0
        : !tail.callIds.includes(callId),
    ),
    sideRequests: {
      ...s.sideRequests,
      settledCalls:
        callId === null || s.sideRequests.settledCalls.includes(callId)
          ? s.sideRequests.settledCalls
          : [...s.sideRequests.settledCalls, callId],
      idleSettled: s.sideRequests.idleSettled || callId === null,
    },
  };
}

export function sideOutput<T extends SideStreaming>(
  s: T,
  kind: string,
  data: OutputData,
): T {
  if (!("text" in data)) return s;
  if (
    kind === "consolidation_delta" ||
    kind === "consolidation_thinking_delta"
  ) {
    if (
      s.sideRequests.consolidation.phase !== "open" ||
      s.consolidation === null
    )
      return s;
    const tail = s.consolidation;
    const field = kind === "consolidation_delta" ? "text" : "thinking";
    return {
      ...s,
      consolidation: { ...tail, [field]: cap(tail[field] + data.text) },
    };
  }
  if (kind === "compaction_delta" || kind === "compaction_thinking_delta") {
    if (s.sideRequests.compaction.phase !== "open") return s;
    const tail = s.compaction ?? { text: "", thinking: "" };
    const field = kind === "compaction_delta" ? "text" : "thinking";
    return {
      ...s,
      compaction: { ...tail, [field]: cap(tail[field] + data.text) },
    };
  }
  if (
    (kind !== "triage_delta" && kind !== "triage_thinking_delta") ||
    !("call_ids" in data)
  )
    return s;
  if (s.sideRequests.triage.phase !== "open") return s;
  const callIds = [...new Set(data.call_ids)].sort();
  if (
    callIds.length === 0
      ? s.sideRequests.idleSettled
      : callIds.some((id) => s.sideRequests.settledCalls.includes(id))
  )
    return s;
  const at = s.triage.findIndex(
    (tail) =>
      tail.callIds.length === callIds.length &&
      tail.callIds.every((id, i) => id === callIds[i]),
  );
  const tail = s.triage[at] ?? { callIds, text: "", thinking: "" };
  const field = kind === "triage_delta" ? "text" : "thinking";
  const next = { ...tail, [field]: cap(tail[field] + data.text) };
  return {
    ...s,
    triage:
      at === -1
        ? [...s.triage, next]
        : s.triage.map((tail, i) => (i === at ? next : tail)),
  };
}
