// The one fetch wrapper (docs/control-api.md). The control endpoint is an
// opaque base the client joins paths onto (api/base.ts): `""` at a session
// daemon's root, `/sessions/<id>` when the host serves this page under a session
// prefix. Same-origin either way, so the session cookie rides automatically.
// Every non-2xx carries the standard error envelope; it surfaces as a thrown
// ApiError so callers branch on `code`, not status prose.

import { apiBase, join } from "./base.ts";
import type {
  Attachment,
  CallId,
  Decision,
  ErrorEnvelope,
  LogEntry,
  Process,
  ProcessRecord,
  ReviewLevel,
  Seq,
  StateSnapshot,
  ToolEntry,
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
 * root under either binding. Exported because those surfaces are not all the
 * session's own: `/projects` and `POST /sessions` are the host's, and a module
 * that addresses them joins no base (api/sessionForm.ts). */
export async function requestAt<T>(
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
  await throwIfFailed(response);
  return (await response.json()) as T;
}

/** Throw a non-2xx as an ApiError carrying its error envelope. */
async function throwIfFailed(response: Response): Promise<void> {
  if (response.ok) return;
  let envelope: ErrorEnvelope;
  try {
    envelope = (await response.json()) as ErrorEnvelope;
  } catch {
    envelope = { code: "internal", message: `http ${response.status}` };
  }
  throw new ApiError(response.status, envelope);
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
// A body on DELETE is unusual and deliberate in the one place it is used: the
// push unsubscribe names its subscription by an absolute endpoint URL, which is
// not a path segment.
export const del = <T>(path: string, body?: unknown) =>
  request<T>("DELETE", path, body);

// ---- the routes the client uses today; grows with the ui ----

export const fetchState = () => get<StateSnapshot>("/v1/state");
// `base` pins the read to one session regardless of where the page has since
// moved (api/stream.ts: the connection's own base).
export const fetchLogTail = (n: number, base: string = apiBase()) =>
  requestAt<LogEntry[]>("GET", `${base}/v1/log?tail=${n}`);
// One entry by seq — the dereference address for what the window and the stream
// elide (a truncated result's `content`, docs/control-api.md). ETag-validated, so
// a second look is a 304.
export const fetchLogEntry = (seq: Seq) => get<LogEntry>(`/v1/log/${seq}`);
// `acceptColdRead` is the consent a `409 cold` asks for (docs/control-api.md):
// the session's cache is believed expired, so the turn pays an uncached read.
export const submitTurn = (
  text: string,
  submissionId: string,
  attachments: Attachment[] = [],
  acceptColdRead = false,
) =>
  post<{ status: string; submission_id: string }>("/v1/turns", {
    text,
    submission_id: submissionId,
    // The web client is not the operator at a terminal; name where the turn
    // came from so the model can attribute it (docs/control-api.md).
    source: "web",
    ...(attachments.length > 0
      ? { attachments: attachments.map((a) => a.name) }
      : {}),
    ...(acceptColdRead ? { accept_cold_read: true } : {}),
  });
// Withdraw the pending-queue suffix beginning at this submission (docs/control-api.md —
// recall). The answer is authoritative: the loop may have dequeued the turn already, so
// `withdrawn` can be shorter than the queue showed, or empty. A caller splices only this.
export interface WithdrawnTurn {
  submission_id: string;
  text: string;
  attachments?: Attachment[];
}
export const withdrawTurns = (submissionId: string) =>
  del<{ withdrawn: WithdrawnTurn[] }>(
    `/v1/turns/${encodeURIComponent(submissionId)}`,
  );

// Attachments (docs/inbox.md): a file is uploaded when it is staged, so the
// daemon's description of it — the one the model will read — is known while it
// is still removable; the turn then names it. Raw bytes rather than a JSON body:
// `application/octet-stream` is the one other type the listener's CSRF gate
// accepts, since a cross-site fetch carrying it is preflighted (docs/web-bind.md).
export async function uploadAttachment(
  name: string,
  body: Blob,
): Promise<Attachment> {
  const response = await fetch(
    join(`/v1/attachments?name=${encodeURIComponent(name)}`),
    {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body,
    },
  );
  await throwIfFailed(response);
  return (await response.json()) as Attachment;
}
// A staged upload taken back before sending. A sent one stays: its turn names it.
// A 204 has no body for `request` to parse, hence the bare fetch.
export async function removeAttachment(name: string): Promise<void> {
  await throwIfFailed(
    await fetch(join(`/v1/attachments/${encodeURIComponent(name)}`), {
      method: "DELETE",
    }),
  );
}
/** Where a stored attachment's bytes are — an `<img src>` for an image. */
export const attachmentUrl = (name: string) =>
  join(`/v1/attachments/${encodeURIComponent(name)}`);
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
// `message` names the case; a session whose cache is believed expired answers
// `409 cold` unless the continue carries `acceptColdRead`.
export const continueLoop = (acceptColdRead = false) =>
  post<{ status: string }>(
    "/v1/loop/continue",
    acceptColdRead ? { accept_cold_read: true } : {},
  );
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
// One tool's definition, by the wire name a call carries (docs/control-api.md).
// The catalog is snapshotted before the loop starts and never changes, so this
// revalidates cheaply; `404 not_found` covers both an unknown name and a
// recorded session opened with no daemon behind it, which is absence rather
// than an error state.
export const fetchToolEntry = (name: string) =>
  get<ToolEntry>(`/v1/tools/${encodeURIComponent(name)}`);
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

// Web push (docs/web-push.md). The key is the daemon's derived VAPID public
// key, base64url of an uncompressed P-256 point — `applicationServerKey` wants
// the raw bytes, so the caller decodes it before subscribing.
export const fetchPushKey = () => get<{ key: string }>("/v1/push");
// The browser's `PushSubscription.toJSON()` shape is what the daemon accepts,
// so this forwards it unreshaped. An upsert on the endpoint: a page that
// re-registers leaves one entry.
export const subscribePush = (subscription: PushSubscriptionJSON) =>
  post<{ status: string }>("/v1/push/subscriptions", subscription);
// Forget one subscription. Always 200 — an unsubscribe names an end state, and
// the browser may have already dropped it.
export const unsubscribePush = (endpoint: string) =>
  del<{ status: string }>("/v1/push/subscriptions", { endpoint });

// Trade a TOTP code for the session cookie (docs/web-bind.md). Outside /v1 and
// outside the base: auth is the *listener's* surface, so it is at the root under
// either binding, and one credential covers every session behind a host.
export const mintSession = (code: string) =>
  requestAt<{ status: string }>("POST", "/auth/session", { code });

/** One entry of the host's session listing (docs/web-client.md). The id is the
 * path segment a client joins its control base onto; the rest is what a list
 * surface can show without one `GET /sessions/<id>/v1/state` per row. */
export interface SessionEntry {
  id: string;
  // The session's current title, once one has been written (docs/session-title.md).
  title?: string;
  cwd?: string;
  created_by?: string;
  /** What the session is doing, in one word. */
  status: "running" | "idle" | "gated" | "asking" | "stalled";
  /** Live background processes the session holds; archiving it reaps them. */
  processes: number;
}

export interface ArchivedSession {
  id: string;
  cwd: string;
  title?: string;
  created_at?: number;
  last_active_at?: number;
  problem?: string;
  state: "archived";
}

// A host's one endpoint of its own (docs/web-client.md), at its root rather than
// under the base — a client asking "which sessions are there" is asking the
// listener, not a session. A session daemon serves no such route, which is how
// the client tells the two bindings apart (session/entry.ts).
export const fetchSessions = () =>
  requestAt<SessionEntry[]>("GET", "/sessions");

// Archive a hosted session (docs/control-api.md). The host answers once teardown has
// run — background processes reaped, scratch removed — so the listing that
// follows shows the finished state rather than a session on its way out.
export const archiveSession = (id: string) =>
  requestAt<{ id: string; state: string }>(
    "DELETE",
    `/sessions/${encodeURIComponent(id)}`,
  );

// Fetch archived sessions from the host (docs/control-api.md). When cwd is provided,
// scopes the listing to that working root; otherwise returns archived sessions across
// remembered roots.
export const fetchArchivedSessions = (cwd?: string) => {
  const query = cwd
    ? `?state=archived&cwd=${encodeURIComponent(cwd)}`
    : "?state=archived";
  return requestAt<ArchivedSession[]>("GET", `/sessions${query}`);
};

// Resume an archived session by patching its state to active (docs/resumption.md).
export const resumeSession = (id: string, cwd: string) =>
  requestAt<{ state: string }>("PATCH", `/sessions/${encodeURIComponent(id)}`, {
    state: "active",
    cwd,
  });

// Replace the session's earlier history with an account its own model writes
// (docs/control-api.md). Accepted means the model is writing it: progress is the
// `fold` topic, and the Boundary mark landing in the log is the result. A cold
// session folds by default; `warm` is the explicit choice to fold one whose
// cache is believed live, giving up the cached prefix. A moment with no fold in
// it answers `409 not_foldable` with a message naming the case.
export const foldLoop = (warm = false) =>
  post<{ status: string }>("/v1/loop/fold", warm ? { warm: true } : {});
