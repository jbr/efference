// The one fetch wrapper (docs/control-api.md). The control endpoint is an
// opaque base the client joins paths onto — same-origin here, so the base is
// "" and the session cookie rides automatically. Every non-2xx carries the
// standard error envelope; it surfaces as a thrown ApiError so callers branch
// on `code`, not status prose.

import type {
  CallId,
  Decision,
  ErrorEnvelope,
  LogEntry,
  Process,
  ReviewLevel,
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

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(path, {
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
export const submitTurn = (text: string) =>
  post<{ status: string }>("/v1/turns", { text });
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
// `background: true` on the foreground process is the handoff — expect 202,
// the flip lands at the detach race (docs/control-api.md); `note` rides it as
// the next user message. `muted` is the dismiss toggle.
export const patchProcess = (
  id: number,
  body: { background?: boolean; muted?: boolean; note?: string },
) => patch<Process | null>(`/v1/processes/${id}`, body);
export const killProcess = (id: number) =>
  del<{ status: string }>(`/v1/processes/${id}`);
// Remove a pending notification before it reaches a triage gate (docs/inbox.md).
export const dismissNotification = (id: number) =>
  del<{ status: string }>(`/v1/notifications/${id}`);

// Trade a TOTP code for the session cookie (docs/web-bind.md). Outside /v1:
// auth is the bind's surface, not the session's.
export const mintSession = (code: string) =>
  post<{ status: string }>("/auth/session", { code });
