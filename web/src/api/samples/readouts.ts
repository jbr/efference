// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The topics that read a session out: its usage, the policy in force, its identity and its
// status. The bytes are the ones src/daemon/test/topic_bytes.rs pins; a pin that changes in
// Rust is changed here too.

import type {
  RoleView,
  SessionIdentity,
  StatusView,
  UsageView,
} from "../types.ts";

export const readouts: {
  usage: UsageView[];
  role: RoleView[];
  sessionIdentity: SessionIdentity[];
  status: StatusView[];
} = {
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
};
