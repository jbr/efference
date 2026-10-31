// The control stream's frames and the state they fold into (docs/control-api.md).
// Every frame is `{class, kind, data}`; only `log` frames carry a `seq`.

import type { LogEntry, Seq } from "./entry.ts";
import type { StateTopics, StreamDelta } from "./types.ts";

/** The name of a state topic: a key of `GET /state` other than the log cursor. */
export type TopicName = Exclude<keyof StateTopics, "last_seq">;

/** The topics a client has received, each the value of its latest `state` frame.
 * A topic is absent until its first frame; `null` is a cleared topic. */
export type TopicValues = Partial<Omit<StateTopics, "last_seq">>;

/** `GET /state`, or the fold of the frames since connect: the log cursor, and the
 * topics that have arrived. */
export interface StateSnapshot extends TopicValues {
  last_seq: Seq | null;
}

/** A `state` frame: the whole of one topic's value, replacing the last. */
export type StateFrame = {
  [K in TopicName]: { class: "state"; kind: K; data: StateTopics[K] };
}[TopicName];

/** A `log` frame: a committed entry, byte-identical to its `GET /log` element. */
export interface LogFrame {
  class: "log";
  kind: string;
  seq: Seq;
  data: LogEntry;
  /** The daemon-side human-wire render document, beside `data` (unemitted
   * today; the field is reserved). */
  render?: unknown;
}

/** An `output` frame: a fragment of a response, a tool, a process or a side
 * request that is still being written. Ephemeral and lossy; the committed entry
 * replaces it. */
export type OutputFrame = { class: "output" } & StreamDelta;

export type Frame = LogFrame | OutputFrame | StateFrame;
