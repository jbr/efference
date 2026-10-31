// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The state topics, as `GET /state` and the `state` frames carry them. The bytes are
// the ones src/daemon/test/topic_bytes.rs pins; a pin that changes in Rust is changed here
// too.

import type {
  AskView,
  Cold,
  CompactionFailure,
  ConfigRepair,
  CurrentOperations,
  Gate,
  LoopPhase,
  LoopResource,
  NewerVersion,
  PaceView,
  PendingNotification,
  PendingProposal,
  QueuedTurn,
  ReviewLevel,
  Stall,
  StateTopics,
  Verbs,
} from "../types.ts";

export const topics: {
  state: StateTopics[];
  loop: LoopResource[];
  ask: AskView[];
  queued: QueuedTurn[][];
  pace: PaceView[];
  verbs: Verbs[];
  cold: Cold[];
  currentOperations: CurrentOperations[];
  compactionFailure: CompactionFailure[];
  newerVersion: NewerVersion[];
  gate: Gate[];
  notifications: PendingNotification[][];
  proposals: PendingProposal[][];
  loopPhase: LoopPhase[];
  reviewLevel: ReviewLevel[];
  stall: Stall[];
  configRepair: ConfigRepair[];
} = {
  state: [
    {
      ask: null,
      cold: null,
      compaction_failure: null,
      config_repair: null,
      current_operations: null,
      gate: null,
      last_seq: null,
      loop_phase: { phase: "idle" },
      newer_version: null,
      notifications: [],
      pace: { held: false, speed_ms: 0 },
      processes: [],
      proposals: [],
      queued: [],
      resolved_review_level: "spotter_human",
      review_level: "spotter_human",
      role: {
        commands: {},
        paths: {},
        scope: null,
        source: null,
        tools: {},
        watch_for: [],
      },
      session_identity: null,
      stall: null,
      starting: null,
      status: { condition: { kind: "idle" }, footer: {}, id: "" },
      supervisor: null,
      triage_upstream_failure: null,
      upstream_failure: null,
      upstream_retry: null,
      usage: {
        cold_streak: 0,
        context: null,
        cost: null,
        cumulative: {
          cache_creation_input_tokens: 0,
          cache_read_input_tokens: 0,
          input_tokens: 0,
          output_tokens: 0,
        },
        keepalive_skipped: null,
        last: null,
        last_cache_miss: null,
        last_request_at: null,
        plan: null,
        retention: {
          kept_lines: 0,
          shown_lines: 0,
          truncated_lines: 0,
          withheld: 0,
        },
        triage_cold_streak: null,
        triage_keepalive_skipped: null,
        triage_plan: null,
      },
      verbs: {
        compact: {
          accepted: false,
          message:
            "nothing to compact: the conversation is already shorter than what a compaction keeps verbatim",
        },
        continue: {
          accepted: false,
          message: "nothing to continue: the session has no turns yet",
        },
        turn: { accepted: true, delivery: "now" },
      },
    },
  ],
  loop: [
    {
      loop_phase: { phase: "idle" },
      pace: { held: false, speed_ms: 0 },
      review_level: "spotter_human",
    },
    {
      loop_phase: { phase: "delayed", until: 1754300000000 },
      pace: { held: true, speed_ms: 2500 },
      review_level: "human_only",
    },
  ],
  ask: [
    {
      call_id: "toolu_ask",
      n: 1,
      of: 1,
      question: "Which branch?",
      recalled: [],
    },
    {
      call_id: "toolu_ask",
      n: 2,
      of: 3,
      question: "Which branch?",
      recalled: [
        { submission_id: "sub-1", text: "first" },
        {
          attachments: [
            {
              kind: {
                Text: {
                  bytes: 12,
                  first_line: "hello there",
                  lines: 1,
                  shape: "Prose",
                },
              },
              name: "notes.txt",
              path: "/work/.efference/a/attachments/notes.txt",
            },
          ],
          submission_id: "sub-2",
          text: "second",
        },
      ],
    },
  ],
  queued: [
    [
      { submission_id: "sub-1", text: "first" },
      {
        attachments: [
          {
            kind: {
              Text: {
                bytes: 12,
                first_line: "hello there",
                lines: 1,
                shape: "Prose",
              },
            },
            name: "notes.txt",
            path: "/work/.efference/a/attachments/notes.txt",
          },
        ],
        submission_id: "sub-2",
        text: "second",
      },
    ],
  ],
  pace: [
    { held: false, speed_ms: 0 },
    { held: true, speed_ms: 2500 },
  ],
  verbs: [
    {
      compact: {
        accepted: false,
        message:
          "nothing to compact: the conversation is already shorter than what a compaction keeps verbatim",
      },
      continue: {
        accepted: false,
        message: "nothing to continue: the session has no turns yet",
      },
      turn: { accepted: true, delivery: "now" },
    },
    {
      compact: {
        accepted: false,
        message:
          "nothing to compact: the conversation is already shorter than what a compaction keeps verbatim",
      },
      continue: { accepted: true },
      turn: { accepted: true, delivery: "now" },
    },
    {
      compact: {
        accepted: false,
        message: "the loop is running — compact when it stops",
      },
      continue: {
        accepted: false,
        message: "the loop is already running — there is nothing to hand back",
      },
      turn: { accepted: true, delivery: "queued" },
    },
    {
      compact: { accepted: true, warm: true },
      continue: { accepted: true },
      turn: { accepted: true, delivery: "now" },
    },
    {
      compact: { accepted: true, warm: false },
      continue: { accepted: true },
      turn: { accepted: true, delivery: "now" },
    },
    {
      compact: {
        accepted: false,
        message: "a question is pending — answer it first",
      },
      continue: {
        accepted: false,
        message: "a question is pending — answer it with a turn to continue",
      },
      turn: { accepted: true, delivery: "now" },
    },
  ],
  cold: [
    { context_tokens: null, since: 1754300300000 },
    { context_tokens: 600, since: 1754300300000 },
  ],
  currentOperations: [
    {
      compaction: {
        cut: 3,
        since: 1754300000008,
        state: "waiting",
        warm: true,
      },
      requests: [
        { origin: "turn", request: 0, since: 1754300000000, stage: "sent" },
        {
          origin: "triage",
          request: 1,
          since: 1754300000002,
          stage: "streaming",
        },
        {
          attempt: 2,
          origin: "spot",
          request: 2,
          since: 1754300000004,
          stage: "retrying",
          status: 429,
        },
      ],
      tools: [
        {
          call_id: "toolu_1",
          command: "cargo test",
          intent: "run the suite",
          since: 1754300000006,
          tool: "bash",
        },
        {
          call_id: "toolu_2",
          intent: null,
          since: 1754300000007,
          tool: "read",
        },
      ],
    },
    {
      compaction: {
        cut: 7,
        since: 1754300000001,
        state: "writing",
        warm: false,
      },
      requests: [
        {
          origin: "compaction",
          request: 4,
          since: 1754300000001,
          stage: "sent",
        },
      ],
      tools: [],
    },
  ],
  compactionFailure: [
    { at: 1754300000001, error: "the summary request failed: 529 overloaded" },
  ],
  newerVersion: [
    {
      tag: "v0.2.0",
      update_command: "curl -fsSL https://efference.net/install.sh | sh",
      url: "https://efference.net/releases/v0.2.0",
      version: "0.2.0",
    },
  ],
  gate: [
    { call_id: "toolu_x", input: { command: "ls" }, name: "bash" },
    {
      acting_on: {
        chain: ["child", "grand"],
        cwd: "/work/child",
        input: {
          command: "curl -sI https://example.com",
          intent: "fetch headers",
        },
        intent: "fetch headers",
        name: "bash",
        project: "/work",
        session: "child",
      },
      call_id: "toolu_x",
      escalations: [
        { note: "over to you", source: "session[calm-finch]", to: "parent" },
        { source: "session[parent]", to: null },
      ],
      grant: {
        change: { command: { command: "git push" } },
        label: "always allow git push",
      },
      input: { command: "ls" },
      name: "bash",
      reason: "it reaches the network",
    },
  ],
  notifications: [
    [
      { body: "build failed", seq: 0, source: "ci" },
      { body: "child is idle", seq: 1, source: "session[child]", wake: true },
      { body: "flaky test", slot: "ci:main", source: "ci" },
    ],
  ],
  proposals: [
    [
      {
        change: { path: { path: "/tmp/scratch", read: true, write: false } },
        seq: 2,
      },
    ],
  ],
  loopPhase: [
    { phase: "idle" },
    { phase: "requesting" },
    { phase: "executing" },
    { phase: "delayed", until: 1754300000000 },
    { phase: "gated" },
    { phase: "asking" },
    { phase: "cold_parked" },
    { phase: "blocked", reason: "triage_unavailable" },
    { phase: "blocked", reason: "triage_defect" },
    { phase: "blocked", reason: "config_repair" },
    { phase: "blocked", reason: "pace_held" },
  ],
  reviewLevel: ["autonomous", "supervisor", "spotter_human", "human_only"],
  stall: [
    {
      at: 1754300000000,
      attempts: 12,
      detail: "upstream SSE stream failed mid-stream: connection reset",
      message: "the trajectory stopped: 12 attempts in a row failed upstream",
      reason: "retries_exhausted",
    },
    {
      at: 1754300000000,
      attempts: null,
      detail: "no verdict",
      message: "the triage never produced a usable verdict",
      reason: "triage_defect",
    },
  ],
  configRepair: [
    {
      at: 1754300000000,
      message:
        "config unparseable — the loop is parked until a human repairs the file",
      parse_error: "expected `=`",
      path: "/work/.efference/a/config.toml",
      restore_error: "permission denied",
    },
  ],
};
