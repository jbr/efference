// Values the host sends, written as the types generated from the protocol schema. Each
// is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The topics and the output frames are the bytes the host's tests pin
// (src/daemon/test/topic_bytes.rs, src/daemon/test/frame_bytes.rs); a pin that changes
// in Rust is changed here too. The log entries are the three things the entry view adds
// to a stored entry (src/api/entry.ts), in the shapes src/daemon/entry_view/test.rs
// builds and this project's own sessions write.

import type { LogEntry } from "./entry.ts";
import type { OutputFrame } from "./frames.ts";
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
  RoleView,
  SessionIdentity,
  Stall,
  StateTopics,
  StatusView,
  UsageView,
  Verbs,
} from "./types.ts";

export const samples: {
  state: StateTopics[];
  loop: LoopResource[];
  ask: AskView[];
  queued: QueuedTurn[][];
  usage: UsageView[];
  role: RoleView[];
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
  sessionIdentity: SessionIdentity[];
  status: StatusView[];
  outputFrames: OutputFrame[];
  logEntries: LogEntry[];
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
  usage: [
    {
      cold_streak: 2,
      context: { limit: 200000, used: 42 },
      cost: { unpriced: 1, usd: 0.0421 },
      cumulative: {
        cache_creation_input_tokens: 91,
        cache_read_input_tokens: 3,
        input_tokens: 32,
        output_tokens: 63,
      },
      keepalive_skipped: {
        at: 1754300000002,
        completion_usd: 0.005,
        output_tokens: 10000,
        prefix_tokens: 5000,
        premium_usd: null,
        reason: "discard",
      },
      last: {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 3,
        input_tokens: 1,
        output_tokens: 2,
      },
      last_cache_miss: {
        at: 1754300000001,
        cache_missed_input_tokens: 12280,
        reason: "tools_changed",
      },
      last_request_at: 1754300000000,
      plan: {
        active: "codex",
        limits: {
          codex: {
            name: null,
            primary: {
              resets_at: 1754300060000,
              used_percent: 12.5,
              window_minutes: 300,
            },
            secondary: null,
          },
        },
        observed_at: 1754300000000,
        type: "plus",
      },
      retention: {
        kept_lines: 0,
        shown_lines: 0,
        truncated_lines: 0,
        withheld: 0,
      },
      triage_cold_streak: 0,
      triage_keepalive_skipped: {
        at: 1754300000003,
        detail: "usage limit reached",
        reason: "exhausted",
        resets_at: 1754303600000,
      },
      triage_plan: {
        active: "codex",
        limits: {
          codex: {
            name: null,
            primary: {
              resets_at: 1754300060000,
              used_percent: 12.5,
              window_minutes: 300,
            },
            secondary: null,
          },
        },
        observed_at: 1754300000005,
        type: "plus",
      },
    },
    {
      cold_streak: 0,
      context: { limit: null, used: 12 },
      cost: { unpriced: 0, usd: 0.0 },
      cumulative: {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 7,
        input_tokens: 5,
        output_tokens: 6,
      },
      keepalive_skipped: {
        at: 1754300000002,
        detail: "400 bad request",
        reason: "rejected",
      },
      last: {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 7,
        input_tokens: 5,
        output_tokens: 6,
      },
      last_cache_miss: {
        at: 1754300000001,
        reason: "previous_message_not_found",
      },
      last_request_at: 1754300000000,
      plan: null,
      retention: {
        kept_lines: 0,
        shown_lines: 0,
        truncated_lines: 0,
        withheld: 0,
      },
      triage_cold_streak: 0,
      triage_keepalive_skipped: {
        at: 1754300000003,
        detail: "usage limit reached",
        reason: "exhausted",
        resets_at: null,
      },
      triage_plan: null,
    },
  ],
  role: [
    {
      commands: {},
      paths: {},
      scope: null,
      source: null,
      tools: {},
      watch_for: [],
    },
    {
      commands: {
        cargo: "watch",
        git: "clear",
        "git push": "deny",
        make: "gate",
      },
      paths: {
        "/tmp/scratch": { read: true, write: true },
        src: { read: true, write: false },
      },
      scope: "Fix the parser.",
      source: "/work/efference.toml",
      tools: {
        efference: { enabled: true },
        "efference.read": { granted: true },
        ferritin: {},
        lsp: { enabled: true },
      },
      watch_for: ["deleting tests", "force pushes"],
    },
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
  sessionIdentity: [
    { cwd: "/work", id: "amber-heron", model: "anthropic/claude-opus-4" },
    {
      cwd: "/work",
      id: "amber-heron",
      inference: {
        main: {
          connection: "work",
          model: "anthropic/claude-opus-4",
          provider: "openrouter",
        },
        spotter: {
          connection: "work",
          model: "anthropic/claude-haiku-4-5",
          provider: "openrouter",
        },
        triage: {
          connection: "work",
          model: "anthropic/claude-opus-4",
          provider: "openrouter",
          shared: true,
        },
      },
      model: "anthropic/claude-opus-4",
      successor: "calm-finch",
      title: "Session names: slugs and titles",
    },
  ],
  status: [
    {
      condition: {
        kind: "asking",
        n: 2,
        of: 3,
        question: "Which branch?",
        returned: [
          { text: "first" },
          { source: "session[calm-finch]", text: "second" },
        ],
      },
      footer: {},
      id: "",
    },
    {
      condition: {
        call_id: "toolu_x",
        kind: "gated",
        reviewer: { kind: "operator" },
        summary: "ls",
        tool: "bash",
      },
      footer: {},
      id: "",
    },
    {
      condition: {
        call_id: "toolu_x",
        escalations: [
          { note: "over to you", source: "session[calm-finch]", to: "parent" },
          { source: "session[parent]", to: null },
        ],
        held_because: "it reaches the network",
        kind: "gated",
        reviewer: { kind: "operator" },
        summary: "child's bash `curl -sI https://example.com`",
        tool: "bash",
      },
      footer: {},
      id: "",
    },
    {
      condition: {
        held_triage: false,
        kind: "stalled",
        message: "the trajectory stopped: 12 attempts in a row failed upstream",
      },
      footer: {},
      id: "",
    },
    {
      condition: { kind: "idle" },
      footer: { model: "anthropic/claude-opus-4" },
      id: "amber-heron",
    },
  ],
  logEntries: [
    // A verdict that keeps every line of the result it decides.
    {
      seq: 115,
      turn: 8,
      at: 1792186748514,
      payload: {
        ResultVerdict: {
          for_result: 110,
          selection: { kept_lines: 1, total_lines: 1 },
          wire: {
            Triaged: {
              intent_feedback: "the listing is what was asked for",
              keep: "All",
              note: "one line",
            },
          },
        },
      },
    },
    // A result cut at 16 KiB: a prefix of `content`, the tag that says so, and the line
    // arithmetic against the whole.
    {
      seq: 39,
      turn: 11,
      at: 1789271877600,
      payload: {
        ToolResult: {
          call_id: "toolu_1",
          raw: {
            content: "line 1\nline 2\n",
            content_elided: { kept_bytes: 16384, total_bytes: 25786 },
            is_error: false,
          },
          selection: { kept_lines: 645, total_lines: 645 },
          wire: {
            Triaged: {
              intent_feedback: null,
              keep: "All",
              note: "the whole file",
            },
          },
        },
      },
    },
    // A result with an image: its dimensions, and no bytes.
    {
      seq: 12,
      turn: 3,
      at: 1789271877700,
      payload: {
        ToolResult: {
          call_id: "toolu_2",
          raw: {
            content: "screenshot.png",
            is_error: false,
            images: [{ media_type: "image/webp", width: 40, height: 30 }],
          },
          wire: "Raw",
        },
      },
    },
    // A committed assistant entry whose text has an artifact fence.
    {
      seq: 20,
      turn: 4,
      at: 1789271877800,
      payload: {
        Assistant: {
          output: { Messages: [{ Text: "```mermaid\ngraph TD\n```" }] },
          artifacts: [
            {
              text_block: 0,
              fence: 0,
              kind: "mermaid",
              url: "/artifacts/amber-heron/20/0/0/0123456789abcdef",
              external_url:
                "http://127.0.0.1:7979/amber-heron/20/0/0/0123456789abcdef",
            },
          ],
        },
      },
    },
    // A turn with an attached file.
    {
      seq: 381,
      turn: 41,
      at: 1792977091962,
      source: "web",
      submission_id: "sub-1",
      payload: {
        User: [
          { Text: "what is in this?" },
          {
            Attachment: {
              kind: {
                Image: {
                  bytes: 2730985,
                  format: "png",
                  height: 2334,
                  width: 2816,
                },
              },
              name: "shot.png",
              path: "/work/.efference/amber-heron/attachments/shot.png",
            },
          },
        ],
      },
    },
  ],
  outputFrames: [
    {
      class: "output",
      kind: "assistant_delta",
      data: { index: 0, text: 'say "hi"\n' },
    },
    {
      class: "output",
      kind: "thinking_delta",
      data: { index: 2, text: "hmm — é" },
    },
    {
      class: "output",
      kind: "tool_call_start",
      data: { call_id: "toolu_1", index: 1, name: "bash" },
    },
    {
      class: "output",
      kind: "tool_call_input",
      data: { fragment: '{"command":"ls', index: 1 },
    },
    {
      class: "output",
      kind: "tool_output",
      data: { call_id: "toolu_1", chunk: "a\tb\n" },
    },
    {
      class: "output",
      kind: "process_output",
      data: { chunk: "ready\n", id: 3 },
    },
    {
      class: "output",
      kind: "compaction_delta",
      data: { text: "the summary" },
    },
    {
      class: "output",
      kind: "compaction_thinking_delta",
      data: { text: "weighing" },
    },
    {
      class: "output",
      kind: "consolidation_delta",
      data: { text: "the account" },
    },
    {
      class: "output",
      kind: "consolidation_thinking_delta",
      data: { text: "sorting" },
    },
    {
      class: "output",
      kind: "triage_delta",
      data: { call_ids: ["toolu_1", "toolu_2"], text: "keep 1-3" },
    },
    {
      class: "output",
      kind: "triage_thinking_delta",
      data: { call_ids: [], text: "idle" },
    },
  ],
};
