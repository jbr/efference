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
  /** Who submitted a `User` entry, when not the operator (docs/control-api.md):
   * `web`, `mcp[<name>]`, `agent[<id>]`, or {@link KICKOFF_SOURCE}. */
  source?: string | null;
  payload: Payload;
}

/** The source the harness's `first-turn` kickoff carries (src/frontend.rs). The
 * turn belongs to the session's opening, like the preamble, so the transcript
 * does not draw it (docs/context.md). */
export const KICKOFF_SOURCE = "efference";

/** The predecessor a hand-off brief's `source` names — `handoff[<id>]`, the
 * reserved form `hand_off` writes (src/frontend.rs, docs/tools.md) — or `null`
 * for any other source. The brief is the predecessor speaking, not the operator. */
export function handoffPredecessor(
  source: string | null | undefined,
): string | null {
  const match = /^handoff\[(.+)\]$/.exec(source ?? "");
  return match === null ? null : match[1];
}

export type Payload =
  | { User: UserBlock[] }
  | {
      Assistant: {
        /** The provider's output in provider order, in whichever wire dialect
         * answered (src/log.rs, docs/log-and-projection.md). Nothing in ui/
         * reads this directly: `session/assistant.ts` derives the grouped
         * thinking / prose / calls view the transcript renders. */
        output: AssistantOutput;
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
  | {
      ToolResult: {
        call_id: CallId;
        raw: RawResult;
        wire: WireState;
        /** The verdict's line arithmetic against the *whole* content, computed
         * by the daemon (docs/control-api.md). Present only on a `Triaged`
         * result. The authority for "kept N of M lines" when `raw.content` is
         * a truncated prefix, which cannot be counted client-side. */
        selection?: SelectionLines;
      };
    }
  | {
      /** The decision for a result captured as `Pending` (docs/triage.md
       * "Capture, then verdict"). The tree folds it onto the result's slot;
       * it is never an item of its own. */
      ResultVerdict: {
        /** The seq of the `ToolResult` this decides. */
        for_result: Seq;
        wire: WireState;
        /** The line arithmetic of a `Triaged` wire, against the result's
         * whole content — the same keys a directly-logged result carries. */
        selection?: SelectionLines;
      };
    }
  | { Summary: Summary }
  | { Triage: Triage }
  | { Ambient: Notification }
  | { Spot: Spot }
  | { ConfigEdit: ConfigEdit }
  | { Telemetry: Telemetry }
  | { ProcessEvent: ProcessEvent }
  | { Preamble: { system: string; context: unknown } }
  | { Title: Title }
  | { Resumed: Resumed }
  | { Worktree: WorktreeEvent };

/** One step of the cold check (docs/control-api.md). Not drawn: the `park`
 * topic is the live state. */
export type ColdEvent =
  | {
      event: "parked";
      reason: "expired";
      since: number;
      context_tokens: number | null;
    }
  | {
      event: "released";
      by: "turn" | "approval" | "ask" | "continue";
    }
  | { event: "unparked"; cause: "person" | "warm" };

/** How the session ran rather than what it said (docs/log-and-projection.md):
 * projects to nothing, and the transcript renders none of it. */
export type Telemetry =
  | { Tool: ToolTelemetry }
  | { CallStarted: { call_id: string } }
  | { RequestStage: RequestStage }
  | { Push: unknown }
  | { Cold: ColdEvent }
  | { CompactionRequested: { warm: boolean; cut: Seq } }
  | { CompactionFailed: { error: string } }
  | { InboxExpired: { seq: Seq; shows: number } }
  | { SupervisorSet: { supervisor: string | null } }
  | { Interrupt: { source?: string; control?: string } }
  | {
      Consolidation: {
        from: Seq;
        upto: Seq;
        context: Seq;
        tool_bytes: number;
        outcome: string;
        account_bytes: number;
      };
    };

/** One stage of one upstream request (docs/log-and-projection.md). Telemetry:
 * the transcript does not draw it; the `current_operations` topic folds it. */
export interface RequestStage {
  request: number;
  origin: RequestOrigin;
  stage:
    | "sent"
    | "first_byte"
    | "ended"
    | { attempt_failed: { attempt: number; status: number } }
    | { failed: { reason: string } };
}

/** What a request is for (docs/log-and-projection.md). */
export type RequestOrigin =
  | "turn"
  | "triage"
  | "title"
  | "spot"
  | "keepalive"
  | "triage_primer"
  | "compaction"
  | "consolidation"
  | "aborted_attempt"
  | "unknown";

/** The `current_operations` topic (docs/control-api.md): what the loop is
 * waiting on right now. `null` when nothing is open. Every `since` is an
 * absolute Unix-millis instant; the client subtracts on its own clock. */
export interface CurrentOperations {
  requests: OpenRequest[];
  tools: RunningTool[];
}

export interface OpenRequest {
  request: number;
  origin: RequestOrigin;
  /** `sent`: handed to the transport, no stream event yet. `retrying`: an
   * attempt was refused (`attempt`, `status`) and the transport is trying
   * again. `streaming`: the first stream event arrived. */
  stage: "sent" | "retrying" | "streaming";
  since: number;
  attempt?: number;
  status?: number;
}

export interface RunningTool {
  call_id: CallId;
  tool: string;
  intent: string | null;
  /** The shell command, present for a `bash` call. */
  command?: string;
  since: number;
}

/** A session reopened by a later host process (docs/resumption.md). */
export interface Resumed {
  gap_ms: number;
  /** Background handles (`p#<n>`) spawned with no recorded exit. */
  lost_processes?: string[];
  /** Present only when the log ended uncleanly: the last seq the session builds
   * on. Entries between it and this one are skipped. */
  resumes_after?: Seq;
  /** Calls of the last batch that had not run when the session stopped. */
  pending_calls?: CallId[];
}

/** One event in a session's own worktree (docs/worktrees.md): the Rust
 * `WorktreeEvent` as its JSON arrives — `path` and `branch` beside a `kind`
 * whose `event` tag carries that variant's fields. */
export interface WorktreeEvent {
  path: string;
  branch: string;
  kind: WorktreeEventKind;
}

/** What happened to the worktree. `added` and `restored` carry what
 * `git worktree add` produced, including the project's `post-checkout` hook
 * output; the rest carry their outcome. */
export type WorktreeEventKind =
  | ({ event: "added" } & WorktreeCheckout)
  | ({ event: "restored" } & WorktreeCheckout)
  | { event: "removed"; branch_deleted: boolean }
  | { event: "kept"; reason: string }
  | { event: "remove_failed"; error: string };

/** What `git worktree add` produced. `exit_code` is `null` when a signal ended
 * it; nonzero with an existing directory is a failed `post-checkout` hook. */
export interface WorktreeCheckout {
  base: string;
  exit_code: number | null;
  output: string;
}

/** One title decision (docs/session-title.md). Log metadata: appended even when
 * the triage kept the previous text, so consecutive entries can repeat. */
export interface Title {
  text: string;
  author: "triage" | "operator";
}

/** Why an assistant entry is record-only. Two serializations of the same fact
 * (src/log.rs): a bare string, and the object an entry with stream diagnostics
 * carries. */
export type UpstreamFailureRecord = string | { why: string; stream?: unknown };

/** One assistant turn's ordered output, externally tagged by wire dialect. */
export type AssistantOutput =
  | { Messages: MessagesOutputBlock[] }
  | { Responses: ResponsesOutputItem[] }
  | { Completions: CompletionsMessage };

/** One Chat Completions assistant message, stored as the stream assembled it
 * (src/log/completions.rs). Only `content`, `tool_calls`, and `refusal` are
 * defined by the dialect; every other key is vendor reasoning under the name it
 * arrived on (`reasoning_content`, `reasoning`, `reasoning_details`, ...). */
export type CompletionsMessage = {
  content?: string | { type: string; [extra: string]: unknown }[];
  tool_calls?: CompletionsToolCall[];
  refusal?: string;
  [extra: string]: unknown;
};

export interface CompletionsToolCall {
  id?: string;
  function?: { name?: string; arguments?: string };
  [extra: string]: unknown;
}

/** One content block of an Anthropic-Messages assistant message. */
export type MessagesOutputBlock =
  | { Text: string }
  | { Thinking: { text: string; signature: string } }
  | { RedactedThinking: { data: string } }
  | { ToolUse: ToolCall };

/** One top-level output item of an OpenAI Responses result. Fields the client
 * never reads (`status`, `encrypted_content`, `annotations`) are left open so a
 * provider addition does not fail the parse. */
export type ResponsesOutputItem =
  | {
      Reasoning: {
        id: string;
        summary: { text: string }[];
        content?: { text: string }[];
        [extra: string]: unknown;
      };
    }
  | {
      Message: {
        id: string;
        content: ResponsesMessageContent[];
        [extra: string]: unknown;
      };
    }
  | {
      FunctionCall: {
        call_id: CallId;
        name: string;
        /** The provider's exact JSON-encoded argument string — the replay
         * authority. Parsed on demand for the call view. */
        arguments: string;
        [extra: string]: unknown;
      };
    }
  | { Unsupported: unknown };

export type ResponsesMessageContent =
  | { OutputText: { text: string; [extra: string]: unknown } }
  | { Refusal: { refusal: string } };

export type ContentBlock =
  | { Text: string }
  | { Thinking: { text: string; signature: string } }
  | { RedactedThinking: { data: string } };

/** What a `User` entry carries: prose, and the files its composer attached. */
export type UserBlock = ContentBlock | { Attachment: Attachment };

/** A file a composer attached (docs/inbox.md): what the daemon stored it as,
 * the path the model reads it at, and the triage signals the model's notice
 * states. `name` addresses its bytes at `GET /v1/attachments/:name`. */
export interface Attachment {
  name: string;
  path: string;
  kind: AttachmentKind;
}

export type AttachmentKind =
  | {
      Text: { bytes: number; lines: number; shape: string; first_line: string };
    }
  | { Image: { format: string; width: number; height: number; bytes: number } }
  | { Binary: { bytes: number } };

export interface ToolCall {
  id: CallId;
  name: string;
  input: unknown;
}

export interface SelectionLines {
  total_lines: number;
  kept_lines: number;
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
  /** Captured, triage not yet decided: the result is its raw output until a
   * `ResultVerdict` names it (docs/triage.md). */
  | "Pending"
  | "Raw"
  | {
      Triaged: {
        keep: Keep;
        note: string | null;
        intent_feedback?: string | null;
      };
    }
  | "Elided"
  | { TriageFailed: { why: string } };

export type Keep = "All" | "None" | { Ranges: [number, number][] };

/** The text that replaces the entries `from..=upto` (docs/summary.md).
 * `made_by` is absent for a consolidation; `audience` is absent for `All`. */
export interface Summary {
  from: Seq;
  upto: Seq;
  text: string;
  made_by?: { compaction: unknown };
  audience?: "triage";
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
 * triage has run, so the pair (this, the rendered result) is what
 * says how much triage saved. Additive by construction on the Rust side: fields
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
  /** The `p#<n>` the process monitor minted for this command, when it reached a
   * spawn (docs/process-monitor.md). Absent for a call that never spawned. */
  handle?: string;
}

/** One lifecycle fact about a background process (docs/process-monitor.md): the mint
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
  | {
      decision: "config_edit";
      change: ConfigChange;
      note?: string;
    };

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
   * (docs/process-monitor.md). `null` for a process registered outside a tool call,
   * and absent from a daemon that predates the mapping. */
  call_id?: string | null;
  pid: number;
  spawned_at: number;
  background: boolean;
  muted: boolean;
}

/** One process, running or exited — what `GET /processes/:id` answers with
 * (docs/control-api.md, docs/process-monitor.md). The listing's representation plus
 * the two fields it has no need for, because it carries only running
 * processes. A log-derived answer (no live registry, or a restarted daemon)
 * reads `running: false` and `muted: false`; `exit: null` beside
 * `running: false` says the end was never observed. */
export interface ProcessRecord {
  id: number;
  command: string;
  call_id?: string | null;
  pid?: number | null;
  spawned_at: number;
  background: boolean;
  muted: boolean;
  running: boolean;
  exit?: ProcessExit | null;
}

/** One tool's full definition — what `GET /tools/:name` answers with
 * (docs/control-api.md). `input_schema` is served exactly as the model was sent
 * it, so a `Toolbox`-mounted tool's schema carries the injected `intent`
 * property while the loop-owned family's does not. `server` names the mounting
 * server and is absent for one of the harness's own tools; where it is present,
 * `title`/`description` (and the schema's per-property descriptions) are
 * third-party prose, rendered as plain text and attributed. */
export interface ToolEntry {
  name: string;
  title?: string;
  description?: string;
  input_schema: unknown;
  annotations?: Record<string, unknown>;
  server?: string | null;
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
  // Context-window occupancy (docs/transport.md): `limit` is the window itself —
  // not reduced by the output reservation — and `used` the last request's
  // prompt tokens, null until the first request. `limit` is null when the
  // endpoint reported no window (a custom endpoint).
  context: { limit: number | null; used: number | null } | null;
  cost: { usd: number; unpriced: number } | null;
  // The Codex backend's subscription windows as of the last response that named
  // them (docs/transport.md); null on every other provider. Resets are absolute
  // Unix millis — subtract from the client clock, the daemon never ticks.
  plan: Plan | null;
  // The separate triage client's own windows and cold-read streak
  // (docs/control-api.md); null while the triage borrows the agent's client.
  triage_plan?: Plan | null;
  triage_cold_streak?: number | null;
}

export interface PlanWindow {
  used_percent: number;
  window_minutes: number | null;
  resets_at: number | null;
}

export interface Plan {
  type: string | null;
  active: string | null;
  limits: Record<string, PlanLimit>;
  observed_at: number;
}

export interface PlanLimit {
  name: string | null;
  primary: PlanWindow | null;
  secondary: PlanWindow | null;
}

export interface Pace {
  speed_ms: number;
  held: boolean;
}

export type ReviewLevel =
  "autonomous" | "supervisor" | "spotter_human" | "human_only";

// Latest upstream failure: optional HTTP status, consecutive occurrences and
// error detail. Ephemeral diagnostics (docs/transport.md).
// The daemon clears it on the next 2xx, so a set value names a failure that is
// current rather than one already survived.
export interface UpstreamFailure {
  status: number | null;
  count: number;
  detail: string;
}

// The session's prompt cache is believed expired (docs/control-api.md). `null`
// while it is believed warm. `since` is absolute Unix millis.
export interface Cold {
  since: number;
  context_tokens: number | null;
}

// The loop is stopped at its cold check mid-turn (docs/control-api.md); `null`
// otherwise, including an idle cold session.
// `expired`: waiting for a person to act before the uncached read, or for a compaction.
// `compacted`: a compaction summarized the history while parked, so `cold` is null and a
// turn or continue resumes the loop.
export type Park =
  | { reason: "expired"; since: number; context_tokens: number | null }
  | { reason: "compacted" };

// A compaction — the session's history replaced by a summary its own model writes —
// in progress, or the last one's failure (`POST /loop/compact`, docs/control-api.md).
// `null` otherwise. Independent of `cold`: a compaction can be asked of a warm session.
export type Compaction =
  { state: "writing" } | { state: "failed"; error: string };

// A retry in flight (docs/transport.md). Level-triggered: `null` is the
// all-clear — the attempt that finalized, or exhaustion.
export interface UpstreamRetry {
  // Which loop is retrying. An exhausted triage withholds a verdict; an exhausted
  // turn ends the trajectory.
  surface: "turn" | "triage";
  // The attempt that just failed, 1-based, of the budget the loop will spend.
  attempt: number;
  budget: number;
  // Absolute Unix millis at which the next attempt fires; the client subtracts,
  // so the daemon never re-emits a ticking field.
  next_attempt_at: number;
  // Leads with what is happening — "attempt 2 of 12 failed upstream, retrying". The
  // status code and the upstream's words are `detail`, not the headline.
  message: string;
  detail: string;
}

// A trajectory that stopped short (docs/transport.md). On the turn's failure
// paths the loop returns to idle, so `running: false`, no gate, and an empty
// inbox look exactly like a finished run; this topic is the difference. A
// triage held on the upstream (the `triage_*` reasons) stalls with the
// loop still running (docs/triage.md). Either way `POST
// /loop/continue` retries. Level-triggered: `null` once the work resumes.
export interface Stall {
  // Why it stopped short.
  reason:
    | "retries_exhausted"
    | "request_failed"
    | "upstream_refused"
    | "triage_unavailable"
    | "triage_defect";
  // Attempts spent, where the failure path counts them.
  attempts: number | null;
  // Absolute Unix millis at which the loop gave up.
  at: number;
  // Leads with what happened — the same register the retry topic's `message`
  // uses. The recovery is the client's own gesture, so clients add it.
  message: string;
  // The upstream's own words for the failure that ended the trajectory.
  detail: string;
}

// What this session is: its id and title, the model it was configured with,
// and the directory it runs in (docs/control-api.md). It arrives on the connect
// snapshot and again whenever the title moves.
export interface SessionIdentity {
  // The session id (docs/session-title.md): its directory name, the `--name` a
  // client resolves, the address a peer messages. Fixed at creation.
  id: string;
  // The current title — the last title entry's text — once one is written.
  // The one identity field that moves.
  title?: string;
  // The model id `serve` resolved, verbatim; the provider's own spelling.
  model: string;
  // The session's working directory, absolute. Shortening it against a home
  // directory is presentation (`ui/identity.ts`), not wire.
  cwd: string;
  // The session this one handed its work to with `hand_off` (docs/tools.md),
  // once it has; the client follows it there.
  successor?: string;
  // The host connection behind each role; absent under standalone `serve`.
  inference?: InferenceIdentity;
}

// One role's selection: the host connection name, its provider kind, and the
// native model id the wire carries.
export interface ModelSelection {
  connection: string;
  // `custom-<dialect>` for a described connection (src/api/provider.rs).
  provider:
    | "anthropic"
    | "openrouter"
    | "openai"
    | "openai-codex"
    | "typesafe"
    | "custom-messages"
    | "custom-responses"
    | "custom-completions"
    | "custom-systemone";
  model: string;
}

export interface InferenceIdentity {
  main: ModelSelection;
  spotter: ModelSelection;
  // `shared: true` means triage rides the main client; the selection then
  // repeats main's.
  triage: ModelSelection & { shared: boolean };
}

// One optional field per state topic, each named by its topic; `null` is a
// cleared topic, absence an unsubscribed one.
export interface StateTopics {
  gate?: Gate | null;
  ask?: Ask | null;
  role?: Role | null;
  notifications?: PendingNotification[] | null;
  proposals?: Proposal[] | null;
  processes?: Process[] | null;
  usage?: Usage | null;
  pace?: Pace | null;
  review_level?: ReviewLevel | null;
  // The position in force, which is the dial's own value except that `supervisor`
  // with no live supervisor reviews as `spotter_human` (docs/permissions.md).
  resolved_review_level?: ReviewLevel | null;
  // The session that answers this one's gates under `supervisor`, or null when
  // there is none (docs/permissions.md).
  supervisor?: string | null;
  running?: boolean | null;
  stall?: Stall | null;
  upstream_failure?: UpstreamFailure | null;
  // A separate triage client's own last failure; null while the triage borrows
  // the agent's client (docs/control-api.md).
  triage_upstream_failure?: UpstreamFailure | null;
  upstream_retry?: UpstreamRetry | null;
  cold?: Cold | null;
  park?: Park | null;
  compaction?: Compaction | null;
  session_identity?: SessionIdentity | null;
  current_operations?: CurrentOperations | null;
}

/** The `ask` topic (docs/control-api.md): the question the loop is blocked on.
 * `recalled` holds the turns the daemon withdrew from the queue when it began,
 * in the shape `DELETE /turns/:id` answers with. */
export interface Ask {
  call_id: CallId;
  question: string;
  n: number;
  of: number;
  recalled: {
    submission_id: string;
    text: string;
    attachments?: Attachment[];
  }[];
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
    | "not_compactable"
    | "cold"
    | "unauthorized"
    | "forbidden"
    | "rate_limited"
    | "unsupported_content"
    | "too_large"
    | "internal";
  message: string;
  detail?: unknown;
}
