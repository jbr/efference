// The one fetch wrapper (docs/control-api.md). The control endpoint is an
// opaque base the client joins paths onto (api/base.ts): `""` at a session
// daemon's root, `/sessions/<id>` when the host serves this page under a session
// prefix. Same-origin either way, so the session cookie rides automatically.
// Every non-2xx carries the standard error envelope; it surfaces as a thrown
// ApiError so callers branch on `code`, not status prose.

import { join } from "./base.ts";
import type {
  CallId,
  Decision,
  ErrorEnvelope,
  LogEntry,
  Process,
  ProcessRecord,
  ReviewLevel,
  Seq,
  StateSnapshot,
} from "./types.ts";

export class ApiError extends Error {
  code: ErrorEnvelope["code"];
  status: number;
  detail: unknown;

  constructor(status: number, envelope: ErrorEnvelope) {
    super(envelope.message);
    this.code = envelope.code;
    this.status = status;
    this.detail = envelope.detail;
  }
}

/** A request to a URL as given — the listener's own surfaces, which sit at its
 * root under either binding. */
async function requestAt<T>(
  method: string,
  url: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(url, {
    method,
    // The gate requires application/json on body-bearing methods; harmless on
    // the rest.
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    let envelope: ErrorEnvelope;
    try {
      envelope = (await response.json()) as ErrorEnvelope;
    } catch {
      envelope = { code: "internal", message: `http ${response.status}` };
    }
    throw new ApiError(response.status, envelope);
  }
  return (await response.json()) as T;
}

/** A request to a control path, joined onto this client's base. */
const request = <T>(method: string, path: string, body?: unknown) =>
  requestAt<T>(method, join(path), body);

export const get = <T>(path: string) => request<T>("GET", path);
export const post = <T>(path: string, body: unknown) =>
  request<T>("POST", path, body);
export const put = <T>(path: string, body: unknown) =>
  request<T>("PUT", path, body);
export const patch = <T>(path: string, body: unknown) =>
  request<T>("PATCH", path, body);
export const del = <T>(path: string) => request<T>("DELETE", path);

// ---- the routes the client uses today; grows with the ui ----

export const fetchState = () => get<StateSnapshot>("/v1/state");
export const fetchLogTail = (n: number) => get<LogEntry[]>(`/v1/log?tail=${n}`);
// One entry by seq — the dereference address for what the window and the stream
// elide (a truncated result's `content`, docs/control-api.md). Immutable, so the
// browser caches it and a second look costs nothing.
export const fetchLogEntry = (seq: Seq) => get<LogEntry>(`/v1/log/${seq}`);
export const submitTurn = (text: string, submissionId: string) =>
  post<{ status: string; submission_id: string }>("/v1/turns", {
    text,
    submission_id: submissionId,
  });
// Withdraw the pending-queue suffix beginning at this submission (docs/control-api.md —
// recall). The answer is authoritative: the loop may have dequeued the turn already, so
// `withdrawn` can be shorter than the queue showed, or empty. A caller splices only this.
export const withdrawTurns = (submissionId: string) =>
  del<{ withdrawn: { submission_id: string; text: string }[] }>(
    `/v1/turns/${encodeURIComponent(submissionId)}`,
  );
// The id is a precondition: a 409 gate_moved answer means that call is no
// longer the pending gate, and `detail` carries the current one.
export const resolveApproval = (callId: CallId, decision: Decision) =>
  put<{ status: string }>(
    `/v1/approvals/${encodeURIComponent(callId)}`,
    decision,
  );

// The operator dials (docs/control-api.md): one PATCH can move pace and
// review level together; `pace` is itself partial.
export interface LoopPatch {
  speedMs?: number;
  held?: boolean;
  reviewLevel?: ReviewLevel;
}
export const patchLoop = ({ speedMs, held, reviewLevel }: LoopPatch) =>
  patch<{ pace: unknown; review_level: string; running: boolean }>("/v1/loop", {
    ...(speedMs !== undefined || held !== undefined
      ? { pace: { speed_ms: speedMs, held } }
      : {}),
    ...(reviewLevel !== undefined ? { review_level: reviewLevel } : {}),
  });
// Signals the running tool; it deletes no state.
export const interruptLoop = () =>
  post<{ status: string }>("/v1/loop/interrupt", {});
// Hand control back with no input (docs/control-api.md): the idle loop takes
// another turn on the wire it already has — nothing is submitted and nothing is
// appended, so this is not a turn with empty text. A moment with no continue in
// it answers `409 not_continuable`, which arrives here as an `ApiError` whose
// `message` names the case.
export const continueLoop = () =>
  post<{ status: string }>("/v1/loop/continue", {});
// `background: true` on the foreground process is the handoff — expect 202,
// the flip lands at the detach race (docs/control-api.md); `note` rides it as
// the next user message. `muted` is the dismiss toggle.
export const patchProcess = (
  id: number,
  body: { background?: boolean; muted?: boolean; note?: string },
) => patch<Process | null>(`/v1/processes/${id}`, body);
// What one handle was, running or exited (docs/control-api.md): the live
// registry answers when it holds the process, a fold over the log's lifecycle
// records when it does not. `404 not_found` for an id never minted into the
// background this session — the normal answer for a foreground-only ordinal
// and for a log written before the records existed.
export const fetchProcessRecord = (id: number) =>
  get<ProcessRecord>(`/v1/processes/${id}`);
export const killProcess = (id: number) =>
  del<{ status: string }>(`/v1/processes/${id}`);
// The cold seed for a process's tail. `output` is lossy with no replay
// (docs/control-api.md), so a process already running when the client connects
// has no deltas to build a tail from; with no cursor this returns everything
// retained. `lost` counts bytes dropped at the retention cap — reported rather
// than silently skipped.
export interface ProcessOutputRead {
  id: number;
  output: string;
  cursor: number;
  lost: number;
  running: boolean;
}
export const fetchProcessOutput = (id: number, since?: number) =>
  get<ProcessOutputRead>(
    `/v1/processes/${id}/output${since === undefined ? "" : `?since=${since}`}`,
  );
// Remove a pending notification before it reaches a triage gate (docs/inbox.md).
export const dismissNotification = (id: number) =>
  del<{ status: string }>(`/v1/notifications/${id}`);
// Resolve a pending config-edit proposal (docs/control-api.md): the
// orchestrator-above decides; committed folds the change into the effective
// config, cancelled is audit-only.
export const resolveProposal = (
  seq: number,
  state: "committed" | "cancelled",
) => patch<{ state: string }>(`/v1/proposals/${seq}`, { state });

// Trade a TOTP code for the session cookie (docs/web-bind.md). Outside /v1 and
// outside the base: auth is the *listener's* surface, so it is at the root under
// either binding, and one credential covers every session behind a host.
export const mintSession = (code: string) =>
  requestAt<{ status: string }>("POST", "/auth/session", { code });

/** One entry of the host's live-session listing (docs/host.md). The id is the
 * path segment a client joins its control base onto, and the listing carries
 * nothing else: a session's own state is read through its own API. */
export interface SessionEntry {
  id: string;
}

// The host's one endpoint of its own (docs/host.md), at its root rather than
// under the base — a client asking "which sessions are there" is asking the
// listener, not a session. A session daemon serves no such route, which is how
// the client tells the two bindings apart (session/entry.ts).
export const fetchSessions = () =>
  requestAt<SessionEntry[]>("GET", "/sessions");
