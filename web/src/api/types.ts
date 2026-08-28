// TypeScript mirror of the daemon's /v1 envelope (docs/control-api.md), the
// one place wire shapes are declared client-side. Log payloads mirror the
// serde encodings of src/log.rs: externally tagged enums, so a variant is
// either a bare string (unit) or a single-key object. When the control API
// moves, this file and the frame parser in stream.ts are the whole blast
// radius; nothing in ui/ names a wire field.

// ---- log entries ----

export type Seq = number;
export type CallId = string;

export interface LogEntry {
  seq: Seq;
  turn: number;
  at: number;
  /** The `POST /v1/turns` submission this entry realized — present on every
   * socket-submitted `User` entry (client-minted or daemon-minted), absent on
   * entries the loop originates itself (docs/control-api.md). */
  submission_id?: string;
  payload: Payload;
}

export type Payload =
  | { User: ContentBlock[] }
  | {
      Assistant: {
        thinking: ContentBlock[];
        text: ContentBlock[];
        calls: ToolCall[];
        /** Set on a **record-only** entry: the stream never stood behind it —
         * an aborted partial, an absorbed HTTP failure, an empty finalized
         * stream — so `project` skips it and nothing here is in the model's
         * context (src/log.rs, docs/transport.md). Absent on a finalized turn,
         * which is what every older log carries too. The bare string is the
         * older shape; the object's `why` is the same prose and its `stream`
         * is the empty-stream evidence, which the transcript has no reason to
         * show. */
        upstream_failure?: UpstreamFailureRecord;
      };
    }
  | { ToolResult: { call_id: CallId; raw: RawResult; wire: WireState } }
  | { Mark: Mark }
  | { Triage: Triage }
  | { Ambient: Notification }
  | { Spot: Spot }
  | { ConfigEdit: ConfigEdit }
  | { ToolTelemetry: ToolTelemetry }
  | { ProcessEvent: ProcessEvent }
  | { Preamble: { system: string; context: unknown } };

/** Why an assistant entry is record-only. Two serializations of the same fact
 * (src/log.rs): a bare string, and the object an entry with stream diagnostics
 * carries. */
export type UpstreamFailureRecord = string | { why: string; stream?: unknown };

export type ContentBlock =
  | { Text: string }
  | { Thinking: { text: string; signature: string } }
  | { RedactedThinking: { data: string } };

export interface ToolCall {
  id: CallId;
  name: string;
  input: unknown;
}

export interface RawResult {
  content: string;
  is_error: boolean;
  /** Present only when `content` is a *prefix* (docs/control-api.md): the
   * window and stream views truncate an oversized result past 16 KiB, and
   * `GET /v1/log/{seq}` serves it whole. Absent means the content is whole —
   * the presence of this field is the signal, never a length comparison. */
  content_elided?: ContentElision;
  /** Image references (docs/control-api.md): the view omits the base64 `data`;
   * the bytes are at `GET /v1/log/{seq}/images/{idx}`. Absent when empty. */
  images?: ResultImage[];
}

export interface ContentElision {
  kept_bytes: number;
  total_bytes: number;
}

export interface ResultImage {
  media_type: string;
  width: number;
  height: number;
}

export type WireState =
  | "Raw"
  | { Distilled: { keep: Keep; note: string | null } }
  | "Elided"
  | { TriageFailed: { why: string } };

export type Keep = "All" | "None" | { Ranges: [number, number][] };

export interface Mark {
  kind: { Boundary: { upto: Seq } };
  note: string;
}

export type TriageOrigin = "Model" | "Fallback";

export interface Triage {
  for_result: Seq | null;
  origin: TriageOrigin;
  raw_keep: string;
  inbox: [Seq, "Pin" | "Dismiss"][];
  surfaced: string[];
  retries: string[];
}

export interface Notification {
  source: string;
  body: string;
  slot: string | null;
}

export type Stance = "clear" | "watch" | "gate" | "clear_always";

/** What one tool call cost and how it came out (docs/tools.md) — the harness's
 * own measurement of the dispatch, joined to its call by `call_id`. `duration_ms`
 * excludes the permission cascade, and `result_bytes` is the raw result before
 * the fork peek distills it, so the pair (this, the rendered result) is what
 * says how much a gate saved. Additive by construction on the Rust side: fields
 * may appear here that this client does not read yet. */
export interface ToolTelemetry {
  call_id: CallId;
  tool: string;
  duration_ms: number;
  outcome: ToolOutcome;
  result_bytes: number;
  /** Realized image blocks the result carried; omitted when zero. */
  images?: number;
  bash?: BashTelemetry;
  edit?: EditTelemetry;
}

/** How one call came out (docs/tools.md). `rejected` is the tool running and
 * declining — an anchor that matched nothing, a command that exited non-zero, a
 * path outside a grant. `harnesserror` is the harness failing to run the call at
 * all, which is a defect signal rather than a datum about the model. */
export type ToolOutcome = "applied" | "rejected" | "harnesserror";

export interface BashTelemetry {
  /** The status the result reported — `"1"`, or `"signal"` for a command a
   * signal killed. Absent means the result reported none, which for a completed
   * foreground command is exit zero. */
  exit_status?: string;
  /** Whether the call asked to be backgrounded, in which case `duration_ms`
   * measures the spawn and `result_bytes` the handle message. */
  background: boolean;
  /** The `p#<n>` the supervisor minted for this command, when it reached a
   * spawn (docs/supervisor.md). Absent for a call that never spawned. */
  handle?: string;
}

/** One lifecycle fact about a background process (docs/supervisor.md): the mint
 * when it enters the background namespace, or the exit when it ends. The durable
 * answer to "what was `p#3`" for a reader with no daemon — the transcript already
 * carries both events in prose, so this is a record, not an entry to render. */
export interface ProcessEvent {
  id: number;
  kind:
    | {
        Spawned: {
          command: string;
          call_id?: CallId;
          pid?: number;
          at: number;
        };
      }
    | { Exited: ProcessExit };
}

export interface ProcessExit {
  /** `{"code": 0}`, `"signal"`, or `"unknown"`. */
  outcome: { code: number } | "signal" | "unknown";
  duration_ms: number;
  at: number;
}

/** Read off an edit-shaped call's own arguments, so any tool that spells them
 * this way contributes. */
export interface EditTelemetry {
  operation?: string;
  anchor_bytes?: number;
  content_bytes?: number;
}

export interface Spot {
  call_id: CallId;
  stance: Stance;
  reason: string;
  origin: TriageOrigin;
  probed_prefix?: string;
}

export type ConfigChange =
  | { command: { command: string } }
  | { path: { path: string; read: boolean; write: boolean } }
  | { tool: { name: string } }
  | { scope: { text: string } };

export interface ConfigEdit {
  state: "proposed" | "committed" | "cancelled";
  resolves: Seq | null;
  change: ConfigChange;
  note: string;
}

// ---- approval decisions (the PUT /v1/approvals/:call_id body) ----

export type Decision =
  | { decision: "approve" }
  | { decision: "abort" }
  | { decision: "redirect"; message: string }
  // Allow-with-prefix: approve and persist the change (docs/permissions.md).
  | { decision: "config_edit"; change: ConfigChange; note?: string };

// ---- state topics (the GET /state snapshot ≡ the fold of `state` frames) ----

export interface Gate {
  call_id: CallId;
  name: string;
  input: unknown;
  reason?: string;
}

// The bash prefix policy's value vocabulary (docs/permissions.md `[commands]`):
// `clear` auto-approves, `watch`/`gate` send the move to the gate rung, `deny`
// refuses without a spotter round-trip. `true`/`false` config sugar is already
// resolved to these words by the time `/state` reports them. Distinct from
// [`Stance`] above (the spotter's verdict) — different vocabulary, `deny` has
// no spotter-verdict counterpart (docs/permissions.md: "Deny is reachable
// only from a `[commands]` stance").
export type CommandStance = "clear" | "watch" | "gate" | "deny";

export interface Role {
  source: string | null;
  scope: string;
  watch_for: string[];
  // Key -> stance, not a list of keys: a `[commands]` entry can carry a
  // carve-out (`watch`/`gate`/`deny`) as well as a grant (`clear`).
  commands: Record<string, CommandStance>;
  // Wire tool names (possibly a namespace glob like `ferritin__*`); render
  // through the dotted display mapping, not raw (docs/permissions.md).
  tools: string[];
  paths: Record<string, { read: boolean; write: boolean }>;
}

export interface PendingNotification {
  id: number;
  source: string;
  body: string;
  slot: string | null;
}

export interface Proposal {
  seq: Seq;
  change: ConfigChange;
}

export interface Process {
  id: number;
  command: string;
  /** The `tool_use` id of the call that spawned it — the call↔process mapping
   * (docs/supervisor.md). `null` for a process registered outside a tool call,
   * and absent from a daemon that predates the mapping. */
  call_id?: string | null;
  pid: number;
  spawned_at: number;
  background: boolean;
  muted: boolean;
}

export interface TokenCounts {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
}

export interface Usage {
  last: TokenCounts | null;
  cumulative: TokenCounts;
  last_request_at: number | null;
  context: { limit: number; used: number | null } | null;
  cost: { usd: number; unpriced: number } | null;
}

export interface Pace {
  speed_ms: number;
  held: boolean;
}

export type ReviewLevel = "autonomous" | "spotter_human" | "human_only";

// The last non-success upstream response, with consecutive occurrences of that
// status. Ephemeral transport diagnostics: never log content (docs/transport.md).
// The daemon clears it on the next 2xx, so a set value names a failure that is
// current rather than one already survived.
export interface UpstreamFailure {
  status: number;
  count: number;
}

// A retry in flight (docs/transport.md). Level-triggered: `null` is the
// all-clear — the attempt that finalized, or exhaustion.
export interface UpstreamRetry {
  // Which loop is retrying. An exhausted peek withholds a verdict; an exhausted
  // turn ends the trajectory.
  surface: "turn" | "peek";
  // The attempt that just failed, 1-based, of the budget the loop will spend.
  attempt: number;
  budget: number;
  // Absolute Unix millis at which the next attempt fires; the client subtracts,
  // so the daemon never re-emits a ticking field.
  next_attempt_at: number;
  // Leads with what is happening — "stream aborted, retrying (2 of 12)". The
  // status code and the upstream's words are `detail`, not the headline.
  message: string;
  detail: string;
}

// A trajectory that ended without finishing (docs/transport.md). The loop
// returns to idle on the failure paths that produce it, so `running: false`,
// no gate, and an empty inbox look exactly like a finished run; this topic is
// the difference. Level-triggered: `null` once a trajectory starts again.
export interface Stall {
  // Why it stopped short.
  reason: "retries_exhausted" | "request_failed";
  // Attempts spent, where the failure path counts them.
  attempts: number | null;
  // Absolute Unix millis at which the loop gave up.
  at: number;
  // Leads with what happened and what recovers it — the same register the
  // retry topic's `message` uses.
  message: string;
  // The upstream's own words for the failure that ended the trajectory.
  detail: string;
}

// What this session is: the model it was configured with and the directory it
// runs in (docs/control-api.md). Static per session today — it arrives on the
// connect snapshot — but a topic, so a value that ever moves arrives the same way.
export interface SessionIdentity {
  // The model id `serve` resolved, verbatim; the provider's own spelling.
  model: string;
  // The session's working directory, absolute. Shortening it against a home
  // directory is presentation (`ui/identity.ts`), not wire.
  cwd: string;
}

// One optional field per state topic, each named by its topic; `null` is a
// cleared topic, absence an unsubscribed one.
export interface StateTopics {
  gate?: Gate | null;
  role?: Role | null;
  notifications?: PendingNotification[] | null;
  proposals?: Proposal[] | null;
  processes?: Process[] | null;
  usage?: Usage | null;
  pace?: Pace | null;
  review_level?: ReviewLevel | null;
  running?: boolean | null;
  stall?: Stall | null;
  upstream_failure?: UpstreamFailure | null;
  upstream_retry?: UpstreamRetry | null;
  session_identity?: SessionIdentity | null;
}

export interface StateSnapshot extends StateTopics {
  last_seq: Seq | null;
}

// ---- the /events frame envelope ----

export type Frame =
  // `render` is the daemon-side human-wire render document, an annotation
  // beside `data` (unemitted today; the field is reserved).
  | { class: "log"; kind: string; seq: Seq; data: LogEntry; render?: unknown }
  | { class: "output"; kind: OutputKind; data: OutputData }
  | { class: "state"; kind: keyof StateTopics; data: unknown };

export type OutputKind =
  | "assistant_delta"
  | "thinking_delta"
  | "tool_call_start"
  | "tool_call_input"
  | "tool_output"
  | "process_output";

// Every model-stream delta names the content block it appends to (`index`), and
// a call additionally the `call_id` its result keys on — a fragment is partial
// delivery of a specific item (docs/control-api.md). `tool_output` and
// `process_output` were always keyed this way.
export type OutputData =
  | { index: number; text: string }
  | { index: number; call_id: CallId; name: string }
  | { index: number; fragment: string }
  | { call_id: CallId; chunk: string }
  | { id: number; chunk: string };

// ---- the error envelope ----

export interface ErrorEnvelope {
  code:
    | "bad_request"
    | "invalid_decision"
    | "not_found"
    | "gate_moved"
    | "not_continuable"
    | "unauthorized"
    | "forbidden"
    | "rate_limited"
    | "unsupported_content"
    | "too_large"
    | "internal";
  message: string;
  detail?: unknown;
}
