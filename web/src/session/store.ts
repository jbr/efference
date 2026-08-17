// The client-side session model: a pure fold of control-stream frames over an
// immutable snapshot (docs/control-api.md — `GET /state` ≡ the fold of every
// `state` frame; the log is append-only; `output` is a lossy pre-commit tail).
// Pure functions so the fold is testable without a browser or a daemon;
// Store wraps them with subscription for useSyncExternalStore. Nothing here
// imports React or touches the network.

import type {
  CallId,
  Frame,
  LogEntry,
  OutputData,
  Seq,
  Spot,
  StateTopics,
} from "../api/types.ts";

/** Pre-commit output: what the model is emitting ahead of the next log entry. */
export interface Streaming {
  thinking: string;
  assistant: string;
  /** Set from `tool_call_start`; the phase indicator while input streams. */
  toolName: string | null;
  /** Verbatim partial JSON — displayable, never parseable mid-stream. */
  toolInput: string;
  /** Live tool output by call id, cleared when the result commits. */
  toolOutput: Record<CallId, string>;
  processOutput: Record<number, string>;
}

export type Connection =
  "connecting" | "live" | "reconnecting" | "unauthenticated";

export interface Session {
  connection: Connection;
  /** Committed entries in seq order — the window we've seen, not the whole log. */
  entries: LogEntry[];
  lastSeq: Seq | null;
  topics: StateTopics;
  streaming: Streaming;
}

export const emptyStreaming: Streaming = {
  thinking: "",
  assistant: "",
  toolName: null,
  toolInput: "",
  toolOutput: {},
  processOutput: {},
};

export const emptySession: Session = {
  connection: "connecting",
  entries: [],
  lastSeq: null,
  topics: {},
  streaming: emptyStreaming,
};

/** Cap per streaming buffer; the committed entry is the authoritative copy. */
const BUFFER_CAP = 256 * 1024;

const cap = (s: string) => (s.length > BUFFER_CAP ? s.slice(-BUFFER_CAP) : s);

/** The most recent spotter verdict logged for a call, if any — the stance +
 * reason the gate surface renders beside the pending call. */
export function spotFor(entries: LogEntry[], callId: CallId): Spot | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const p = entries[i].payload;
    if ("Spot" in p && p.Spot.call_id === callId) return p.Spot;
  }
  return null;
}

/** Seed the transcript window from a `GET /log?tail=` read. */
export function seed(session: Session, entries: LogEntry[]): Session {
  const last = entries.at(-1);
  return {
    ...session,
    entries,
    lastSeq: last ? last.seq : session.lastSeq,
  };
}

export function applyFrame(session: Session, frame: Frame): Session {
  switch (frame.class) {
    case "log":
      return applyLog(session, frame.data);
    case "state":
      return {
        ...session,
        topics: { ...session.topics, [frame.kind]: frame.data },
      };
    case "output":
      return {
        ...session,
        streaming: applyOutput(session.streaming, frame.kind, frame.data),
      };
    default:
      // A durability class from a newer daemon: nothing safe to fold.
      return session;
  }
}

function applyLog(session: Session, entry: LogEntry): Session {
  // Replay after reconnect can resend a seq we hold; the fold is idempotent.
  if (session.lastSeq !== null && entry.seq <= session.lastSeq) {
    return session;
  }
  return {
    ...session,
    entries: [...session.entries, entry],
    lastSeq: entry.seq,
    streaming: settle(session.streaming, entry),
  };
}

/**
 * Clear the pre-commit buffers a committed entry supersedes: an assistant
 * entry is the durable copy of the thinking/prose/tool-input tail, a
 * tool_result of its live output.
 */
function settle(streaming: Streaming, entry: LogEntry): Streaming {
  if ("Assistant" in entry.payload) {
    return {
      ...streaming,
      thinking: "",
      assistant: "",
      toolName: null,
      toolInput: "",
    };
  }
  if ("ToolResult" in entry.payload) {
    const toolOutput = { ...streaming.toolOutput };
    delete toolOutput[entry.payload.ToolResult.call_id];
    return { ...streaming, toolOutput };
  }
  return streaming;
}

function applyOutput(
  streaming: Streaming,
  kind: string,
  data: OutputData,
): Streaming {
  switch (kind) {
    case "assistant_delta":
      if ("text" in data) {
        return {
          ...streaming,
          assistant: cap(streaming.assistant + data.text),
        };
      }
      return streaming;
    case "thinking_delta":
      if ("text" in data) {
        return { ...streaming, thinking: cap(streaming.thinking + data.text) };
      }
      return streaming;
    case "tool_call_start":
      if ("name" in data) {
        return { ...streaming, toolName: data.name, toolInput: "" };
      }
      return streaming;
    case "tool_call_input":
      if ("fragment" in data) {
        return {
          ...streaming,
          toolInput: cap(streaming.toolInput + data.fragment),
        };
      }
      return streaming;
    case "tool_output":
      if ("call_id" in data) {
        return {
          ...streaming,
          toolOutput: {
            ...streaming.toolOutput,
            [data.call_id]: cap(
              (streaming.toolOutput[data.call_id] ?? "") + data.chunk,
            ),
          },
        };
      }
      return streaming;
    case "process_output":
      if ("id" in data) {
        return {
          ...streaming,
          processOutput: {
            ...streaming.processOutput,
            [data.id]: cap(
              (streaming.processOutput[data.id] ?? "") + data.chunk,
            ),
          },
        };
      }
      return streaming;
    default:
      return streaming;
  }
}

/** A subscribable holder for the fold — the seam useSyncExternalStore binds to. */
export class Store {
  private session: Session = emptySession;
  private listeners = new Set<() => void>();

  snapshot = (): Session => this.session;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  update(f: (session: Session) => Session): void {
    const next = f(this.session);
    if (next === this.session) return;
    this.session = next;
    for (const listener of this.listeners) listener();
  }
}
