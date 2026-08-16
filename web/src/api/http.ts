// The one fetch wrapper (docs/control-api.md). The control endpoint is an
// opaque base the client joins paths onto — same-origin here, so the base is
// "" and the session cookie rides automatically. Every non-2xx carries the
// standard error envelope; it surfaces as a thrown ApiError so callers branch
// on `code`, not status prose.

import type { ErrorEnvelope, LogEntry, StateSnapshot } from "./types.ts";

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

// ---- the routes the client uses today; grows with the ui ----

export const fetchState = () => get<StateSnapshot>("/v1/state");
export const fetchLogTail = (n: number) => get<LogEntry[]>(`/v1/log?tail=${n}`);
export const submitTurn = (text: string) =>
  post<{ status: string }>("/v1/turns", { text });

// Trade a TOTP code for the session cookie (docs/web-bind.md). Outside /v1:
// auth is the bind's surface, not the session's.
export const mintSession = (code: string) =>
  post<{ status: string }>("/auth/session", { code });
