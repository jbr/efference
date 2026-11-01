// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The `output` frames, one per kind. The bytes are the ones
// src/host/test/frame_bytes.rs pins; a pin that changes in Rust is changed here too.

import type { OutputFrame } from "../frames.ts";

export const frames: {
  outputFrames: OutputFrame[];
} = {
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
