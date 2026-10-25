// The one fetch wrapper (docs/control-api.md). The control endpoint is an
// opaque base the client joins paths onto (api/base.ts): `""` at a session
// daemon's root, `/sessions/<id>` when the host serves this page under a session
// prefix. Same-origin either way, and every request carries the page's bearer
// (api/bearer.ts). Every non-2xx carries the standard error envelope; it
// surfaces as a thrown ApiError so callers branch on `code`, not status prose.

import { apiBase, join } from "./base.ts";
import { bearer } from "./bearer.ts";
import type {
  Attachment,
  CallId,
  Decision,
  ErrorEnvelope,
  LogEntry,
  LoopPhase,
  Process,
  ProcessRecord,
  RecordedSelections,
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

/** The one entry point for every request the client makes (docs/web-client.md):
 * the global `fetch`, with the held bearer attached as `Authorization: Bearer`
 * (api/bearer.ts, docs/web-bind.md). `fetch` is read at call time, so a test
 * that stubs the global is the one that runs, and an eslint rule keeps bare
 * `fetch` out of the rest of `web/src`. Streams and image bytes come through
 * here too.
 *
 * The header joins whatever headers the caller passed, in any form `fetch`
 * takes, and an `Authorization` the caller set is left as it is. It goes only
 * to this page's own origin. With no bearer held, or another origin addressed,
 * the arguments pass through as given. */
export function apiFetch(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const token = bearer.held();
  if (token === null || !ownOrigin(input)) {
    return init === undefined ? fetch(input) : fetch(input, init);
  }
  const headers = new Headers(
    init?.headers ?? (input instanceof Request ? input.headers : undefined),
  );
  if (!headers.has("authorization")) {
    headers.set("authorization", `Bearer ${token}`);
  }
  return fetch(input, { ...init, headers });
}

/** Whether a request addresses the page's own origin, the only one its bearer
 * is for; a URL that does not resolve against the page does not. */
function ownOrigin(input: RequestInfo | URL): boolean {
  const page = globalThis.location;
  if (page === undefined) return false;
  const url = input instanceof Request ? input.url : String(input);
  try {
    return new URL(url, page.href).origin === page.origin;
  } catch {
    return false;
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
  const response = await apiFetch(url, {
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
export async function throwIfFailed(response: Response): Promise<void> {
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
export const fetchLogBefore = (
  before: Seq,
  n: number,
  base: string = apiBase(),
) => requestAt<LogEntry[]>("GET", `${base}/v1/log?before=${before}&tail=${n}`);
export const fetchLogRange = (
  since: Seq,
  before: Seq,
  base: string = apiBase(),
) =>
  requestAt<LogEntry[]>(
    "GET",
    `${base}/v1/log?since=${since}&before=${before}`,
  );
export const fetchCallSeq = (id: CallId, base: string = apiBase()) =>
  requestAt<{ seq: Seq }>(
    "GET",
    `${base}/v1/tool_calls/${encodeURIComponent(id)}`,
  );

// One entry by seq — the dereference address for what the window and the stream
// elide (a truncated result's `content`, docs/control-api.md). ETag-validated, so
// a second look is a 304.
export const fetchLogEntry = (seq: Seq) => get<LogEntry>(`/v1/log/${seq}`);
// `answers` names the question the turn answers, when one was showing: a turn
// naming none is refused while one is pending, so text written before the
// question appeared is never taken as its answer (docs/control-api.md).
export const submitTurn = (
  text: string,
  submissionId: string,
  attachments: Attachment[] = [],
  answers: CallId | null = null,
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
    ...(answers !== null ? { answers } : {}),
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
  base = apiBase(),
): Promise<Attachment> {
  const response = await apiFetch(
    join(`/v1/attachments?name=${encodeURIComponent(name)}`, base),
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
export async function removeAttachment(
  name: string,
  base = apiBase(),
): Promise<void> {
  await throwIfFailed(
    await apiFetch(join(`/v1/attachments/${encodeURIComponent(name)}`, base), {
      method: "DELETE",
    }),
  );
}
/** Where a stored attachment's bytes are — the address `useApiImage` resolves
 * for an image (ui/useApiImage.ts). */
export const attachmentUrl = (name: string, base = apiBase()) =>
  join(`/v1/attachments/${encodeURIComponent(name)}`, base);
// The id is a precondition: a 409 gate_moved answer means that call is no
// longer the pending gate, and `detail` carries the current one. The body names
// this client as the decider, which the host records (docs/control-api.md).
export const resolveApproval = (callId: CallId, decision: Decision) =>
  put<{ status: string }>(`/v1/approvals/${encodeURIComponent(callId)}`, {
    ...decision,
    source: "web",
  });

// The operator dials (docs/control-api.md): one PATCH can move pace and
// review level together; `pace` is itself partial.
export interface LoopPatch {
  speedMs?: number;
  held?: boolean;
  reviewLevel?: ReviewLevel;
}
export const patchLoop = ({ speedMs, held, reviewLevel }: LoopPatch) =>
  patch<{ pace: unknown; review_level: string; loop_phase: LoopPhase }>(
    "/v1/loop",
    {
      ...(speedMs !== undefined || held !== undefined
        ? { pace: { speed_ms: speedMs, held } }
        : {}),
      ...(reviewLevel !== undefined ? { review_level: reviewLevel } : {}),
    },
  );
// Signals the running tool; it deletes no state. The body names this client and
// the affordance, which the host records on an `Interrupt` entry
// (docs/control-api.md).
export type InterruptControl = "stop-button" | "status-bar-stop";
export const interruptLoop = (control: InterruptControl) =>
  post<{ status: string }>("/v1/loop/interrupt", { source: "web", control });
// Hand control back with no input (docs/control-api.md): the idle loop takes
// another turn on the wire it already has — nothing is submitted and nothing is
// appended, so this is not a turn with empty text. A moment with no continue in
// it answers `409 not_continuable`, which arrives here as an `ApiError` whose
// `message` names the case.
// `source` names the web client, as its turns do, so the log records who continued.
export const continueLoop = () =>
  post<{ status: string }>("/v1/loop/continue", { source: "web" });
// `background: true` on the foreground process is the handoff — expect 202,
// the flip lands at the detach race (docs/control-api.md); `note` rides it as
// the next user message. `muted` is the dismiss toggle.
export const patchProcess = (
  id: number,
  body: { background?: boolean; muted?: boolean; note?: string },
) => patch<Process | null>(`/v1/processes/${id}`, body);
// What one handle was, running or exited (docs/control-api.md): the live
// registry answers when it holds the process, a compaction over the log's lifecycle
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
/** One client's report of whether a person is attending, and which session it
 * has open (docs/web-push.md). The host's own surface, at the listener's root
 * under either binding; answered `204`, so nothing is parsed. `keepalive` lets
 * the report that says the page is leaving outlive it. */
export async function sendAttention(report: {
  client: string;
  session: string | null;
  active: boolean;
}): Promise<void> {
  const response = await apiFetch("/attention", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(report),
    keepalive: true,
  });
  await throwIfFailed(response);
}

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

// Trade a TOTP code for a device token, which the caller keeps (api/bearer.ts,
// docs/web-bind.md); `label` is what the host lists the device as. Outside /v1
// and outside the base: auth is the *listener's* surface, so it is at the root
// under either binding, and one credential covers every session behind a host.
export const mintSession = (code: string, label?: string) =>
  requestAt<{ token: string }>("POST", "/auth/session", { code, label });

/** One entry of the host's session listing (docs/web-client.md). The id is the
 * path segment a client joins its control base onto; the rest is what a list
 * surface can show without one `GET /sessions/<id>/v1/state` per row. */
export interface SessionEntry {
  id: string;
  // The session's current title, once one has been written (docs/session-title.md).
  title?: string;
  cwd?: string;
  created_by?: string;
  /** What the session is doing, in one word: `starting` while its environment
   * and MCP servers are prepared (docs/control-api.md). */
  status:
    | "starting"
    | "running"
    | "idle"
    | "gated"
    | "asking"
    | "stalled"
    | "parked"
    | "blocked";
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
  /** Why this host's last attempt to resume the session failed; unlike a
   * `problem`, the session is still resumable (docs/control-api.md). */
  resume_failure?: string;
  state: "archived";
}

/** One dormant session, as `GET /sessions/:id` answers it (docs/control-api.md):
 * the listing's row and what reading its log and usage record learns. The listing
 * itself carries neither, since a scan over every root reads no log whole. */
export interface DormantSession extends ArchivedSession {
  /** The roles the session last ran on, in the flat shape a resume patch takes.
   * Absent when the host could not read the log, or the log records none. */
  selections?: RecordedSelections | null;
  /** The prompt the session's last turn sent: what a cold read would pay.
   * Absent when the usage record holds no `turn` line. */
  context_tokens?: number | null;
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
// The body names this client, as its turns do, so the log records who archived.
export const archiveSession = (id: string) =>
  requestAt<{ id: string; state: string }>(
    "DELETE",
    `/sessions/${encodeURIComponent(id)}`,
    { source: "web" },
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

// One dormant session with what a client offering the model switches reads
// (docs/resumption.md): the roles it last ran on and what its context holds.
export const fetchDormantSession = (id: string) =>
  requestAt<DormantSession>("GET", `/sessions/${encodeURIComponent(id)}`);

// Resume an archived session by patching its state to active (docs/resumption.md).
// `switch` names the roles the resume changes (`src/inference/switch.rs`); its
// fields ride at the top level of the patch, and a role it leaves out keeps the
// selection the log records.
export const resumeSession = (
  id: string,
  cwd: string,
  resumeSwitch: Record<string, unknown> | null = null,
) =>
  requestAt<{ state: string }>("PATCH", `/sessions/${encodeURIComponent(id)}`, {
    state: "active",
    cwd,
    ...(resumeSwitch ?? {}),
  });

// Replace the session's earlier history with a summary its own model writes
// (docs/control-api.md). Accepted means the model is writing it: progress is the
// `compaction` topic, and the Summary entry landing in the log is the result. A cold
// session compacts by default; `warm` is the explicit choice to compact one whose
// cache is believed live, giving up the cached prefix. A moment with no compaction in
// it answers `409 not_compactable` with a message naming the case.
export const compactLoop = (warm = false) =>
  post<{ status: string }>("/v1/loop/compact", warm ? { warm: true } : {});
