// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The bodies a session's routes answer with. The bytes are the ones
// src/host/test/route_bytes.rs pins, with the push key from src/host/test/push.rs and the
// status words from src/protocol/answers/test.rs; a pin that changes in Rust is changed here
// too.

import type {
  FiledNotification,
  LocatedToolCall,
  ProcessOutputRead,
  ProposalResolution,
  PushKey,
  StatusBody,
  TurnAccepted,
  WithdrawnTurns,
} from "../types.ts";

export const routes: {
  turnAccepted: TurnAccepted[];
  withdrawnTurns: WithdrawnTurns[];
  statusBodies: StatusBody[];
  filedNotifications: FiledNotification[];
  processOutput: ProcessOutputRead[];
  proposalResolutions: ProposalResolution[];
  locatedToolCalls: LocatedToolCall[];
  pushKeys: PushKey[];
} = {
  turnAccepted: [
    { delivery: "now", status: "accepted", submission_id: "sub-1" },
    { delivery: "queued", status: "accepted", submission_id: "sub-2" },
  ],
  withdrawnTurns: [
    {
      withdrawn: [
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
    { withdrawn: [] },
  ],
  statusBodies: [
    { status: "accepted" },
    { status: "resolved" },
    { status: "escalated" },
    { status: "interrupted" },
    { status: "continuing" },
    { status: "compacting" },
    { status: "killed" },
    { status: "subscribed" },
    { status: "unsubscribed" },
    { status: "pending" },
  ],
  filedNotifications: [{ seq: 0 }, {}],
  processOutput: [
    { cursor: 7, id: 1, lost: 0, output: "seeded\n", running: false },
    { cursor: 7, id: 1, lost: 0, output: "", running: false },
  ],
  proposalResolutions: [{ state: "committed" }, { state: "cancelled" }],
  locatedToolCalls: [
    {
      call: { id: "toolu_9", input: { command: "cargo test" }, name: "bash" },
      seq: 1,
    },
  ],
  pushKeys: [
    {
      key: "BEl62iUYgUivxIkv69yViEuiBIa-Ib9-SkvMeAtA3LFgDzkrxZJjSgSnfckjBJuBkr3qBUYIHBQFLXYp5Nksh8U",
    },
  ],
};
