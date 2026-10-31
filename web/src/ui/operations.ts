// The composer status bar's account of what the loop is waiting on, from the
// `current_operations` topic (docs/web-client.md). Pure: the clock is a
// parameter, because the topic carries absolute instants and no ticking field.

import type {
  CurrentOperations,
  OpenRequest,
  RequestOrigin,
  RunningTool,
} from "../api/types.ts";

/** Which request or tool leads when several are open: the turn's own request,
 * then a running tool, then the side requests by how much they hold the loop. */
const REQUEST_ORDER: RequestOrigin[] = [
  "turn",
  "triage",
  "spot",
  "compaction",
  "consolidation",
  "title",
  "triage_primer",
  "keepalive",
  "triage_keepalive",
];

/** "45s", "2m 10s", "1h 05m": whole seconds up to a minute, then minutes. */
export function elapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${(s % 60).toString().padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${(m % 60).toString().padStart(2, "0")}m`;
}

const rank = (origin: RequestOrigin) => {
  const i = REQUEST_ORDER.indexOf(origin);
  return i === -1 ? REQUEST_ORDER.length : i;
};

function requestText(request: OpenRequest, model: string, now: number): string {
  const since = elapsed(now - request.since);
  const retrying =
    request.stage === "retrying"
      ? `retrying upstream · attempt ${request.attempt} (${request.status}) · ${since}`
      : null;
  switch (request.origin) {
    case "turn":
    case "aborted_attempt":
    case "unknown":
      if (retrying !== null) return retrying;
      if (request.stage === "streaming") return `${model} streaming`;
      return `waiting on ${model} · ${since}`;
    case "triage":
      return (
        retrying ??
        `triage · ${request.stage === "sent" ? "waiting " : "streaming "}${since}`
      );
    case "spot":
      return `spotter judging · ${since}`;
    case "title":
      return `writing a title · ${since}`;
    case "consolidation":
      return `considering consolidation · ${since}`;
    case "compaction":
      return `writing the summary · ${since}`;
    case "triage_primer":
    case "keepalive":
    case "triage_keepalive":
      return `warming the cache · ${since}`;
  }
}

/** One line for the leading open operation, or null when none is open. */
export function operationText(
  operations: CurrentOperations | null,
  model: string,
  now: number,
): string | null {
  if (operations === null) return null;
  const requests = [...operations.requests].sort(
    (a, b) => rank(a.origin) - rank(b.origin) || a.since - b.since,
  );
  const lead = requests.at(0);
  // The turn's own request leads; any other request yields to a running tool,
  // which is what the operator is waiting on when both are open.
  if (lead !== undefined && lead.origin === "turn") {
    return requestText(lead, model, now);
  }
  const tool = operations.tools.at(0);
  if (tool !== undefined) {
    return `${toolLabel(tool)} · ${elapsed(now - tool.since)}`;
  }
  return lead === undefined ? null : requestText(lead, model, now);
}

/** A running tool's name and what it is doing: a bash call's command, which
 * says more than the intent's sentence in less room, else the intent. */
function toolLabel(tool: RunningTool): string {
  const what = tool.command ?? tool.intent ?? "";
  const label = what.length > 48 ? `${what.slice(0, 47)}…` : what;
  return `${tool.tool}${label === "" ? "" : `: ${label}`}`;
}

/** Hover text for the readout while a bash call leads: the intent the label
 * gave up for the command. Null when the label already is the intent. */
export function operationTitle(
  operations: CurrentOperations | null,
): string | undefined {
  if (operations === null) return undefined;
  const leadsWithTurn = operations.requests.some((r) => r.origin === "turn");
  const tool = operations.tools.at(0);
  if (leadsWithTurn || tool === undefined || tool.command === undefined) {
    return undefined;
  }
  return tool.intent ?? undefined;
}
