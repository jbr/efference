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
      };
    }
  | { ToolResult: { call_id: CallId; raw: RawResult; wire: WireState } }
  | { Mark: Mark }
  | { Triage: Triage }
  | { Ambient: Notification }
  | { Spot: Spot }
  | { ConfigEdit: ConfigEdit }
  | { Preamble: { system: string; context: unknown } };

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
  /** Image references (docs/control-api.md): the view omits the base64 `data`;
   * the bytes are at `GET /v1/log/{seq}/images/{idx}`. Absent when empty. */
  images?: ResultImage[];
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

export interface Role {
  source: string | null;
  scope: string;
  watch_for: string[];
  commands: string[];
  tools: Record<string, unknown>;
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
    | "unauthorized"
    | "forbidden"
    | "rate_limited"
    | "unsupported_content"
    | "too_large"
    | "internal";
  message: string;
  detail?: unknown;
}
