// The client-side session model: a pure fold of control-stream frames over an
// immutable snapshot (docs/control-api.md — `GET /state` ≡ the fold of every
// `state` frame; the log is append-only; `output` is a lossy pre-commit tail).
// Pure functions so the fold is testable without a browser or a daemon;
// Store wraps them with subscription for useSyncExternalStore. Nothing here
// imports React or touches the network.

import type {
  CallId,
  ContentBlock,
  Frame,
  LogEntry,
  OutputData,
  Seq,
  Spot,
  StateTopics,
} from "../api/types.ts";

/** One content block of the in-flight response, identified by its wire `index`
 * (docs/control-api.md). A block is the unit a delta appends to, so holding a
 * list of them — rather than one buffer per kind — is what lets a response with
 * two tool calls show both: with a single slot the second call's start wiped the
 * first, and the arguments already on screen vanished. */
export interface LiveBlock {
  index: number;
  kind: "thinking" | "text" | "call";
  /** Prose or thinking text; for a call, its verbatim partial JSON input —
   * displayable, never parseable mid-stream. */
  text: string;
  /** Calls only: the `tool_use` id its result and annotations key on. */
  callId?: CallId;
  /** Calls only. */
  name?: string;
}

/** Pre-commit output: what the model is emitting ahead of the next log entry. */
export interface Streaming {
  /** The in-flight response's blocks, in wire order. */
  blocks: LiveBlock[];
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
  /** Turns the daemon accepted (a 202) whose `User` entries have not streamed
   * back yet — the operator's delivery signal (docs/web-client.md). Ordered;
   * an arriving `User` entry drops the first content match, the TUI's
   * reconciliation rule (the submit response carries only a status, so there
   * is nothing better to key on). */
  pendingTurns: string[];
}

export const emptyStreaming: Streaming = {
  blocks: [],
  toolOutput: {},
  processOutput: {},
};

export const emptySession: Session = {
  connection: "connecting",
  entries: [],
  lastSeq: null,
  topics: {},
  streaming: emptyStreaming,
  pendingTurns: [],
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

/** Queue a turn the daemon just accepted; `applyLog` settles it when the
 * matching `User` entry arrives. `sinceSeq` is the log cursor from before the
 * submit: the daemon commits the `User` entry and broadcasts its frame
 * independently of writing the 202 body, so the entry can fold *before* the
 * submit response resolves — queueing then would strand a pending copy nothing
 * will ever settle. An entry newer than the pre-submit cursor with this
 * content means the turn is already delivered, so there is nothing to queue. */
export function pushPendingTurn(
  session: Session,
  text: string,
  sinceSeq: Seq | null,
): Session {
  const delivered = session.entries.some(
    (e) =>
      (sinceSeq === null || e.seq > sinceSeq) &&
      "User" in e.payload &&
      userText(e.payload.User) === text,
  );
  if (delivered) return session;
  return { ...session, pendingTurns: [...session.pendingTurns, text] };
}

const userText = (blocks: ContentBlock[]) =>
  blocks.map((b) => ("Text" in b ? b.Text : "")).join("\n\n");

function settlePendingTurns(pending: string[], entry: LogEntry): string[] {
  if (!("User" in entry.payload)) return pending;
  const at = pending.indexOf(userText(entry.payload.User));
  if (at === -1) return pending;
  return [...pending.slice(0, at), ...pending.slice(at + 1)];
}

/** Seed the transcript window from a `GET /log?tail=` read. Entries beyond the
 * cursor are new facts arriving outside `applyLog`, so they settle pending
 * turns here the way a streamed frame would — a re-seed (retry after login,
 * a fresh catch-up read) must not strand a queued turn whose `User` entry it
 * absorbed. */
export function seed(session: Session, entries: LogEntry[]): Session {
  const last = entries.at(-1);
  const pendingTurns = entries
    .filter((e) => session.lastSeq === null || e.seq > session.lastSeq)
    .reduce(settlePendingTurns, session.pendingTurns);
  return {
    ...session,
    entries,
    lastSeq: last ? last.seq : session.lastSeq,
    pendingTurns,
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
    pendingTurns: settlePendingTurns(session.pendingTurns, entry),
  };
}

/**
 * Clear the pre-commit buffers a committed entry supersedes: an assistant
 * entry is the durable copy of the thinking/prose/tool-input tail, a
 * tool_result of its live output.
 */
function settle(streaming: Streaming, entry: LogEntry): Streaming {
  if ("Assistant" in entry.payload) {
    return { ...streaming, blocks: [] };
  }
  if ("ToolResult" in entry.payload) {
    const toolOutput = { ...streaming.toolOutput };
    delete toolOutput[entry.payload.ToolResult.call_id];
    return { ...streaming, toolOutput };
  }
  return streaming;
}

/** Append to the block at `index`, opening it if this is its first delta. The
 * block list stays sorted by index, which is wire order. */
function appendBlock(
  streaming: Streaming,
  index: number,
  kind: LiveBlock["kind"],
  text: string,
): Streaming {
  const at = streaming.blocks.findIndex((b) => b.index === index);
  if (at === -1) {
    return {
      ...streaming,
      blocks: [...streaming.blocks, { index, kind, text: cap(text) }].sort(
        (a, b) => a.index - b.index,
      ),
    };
  }
  const blocks = [...streaming.blocks];
  blocks[at] = { ...blocks[at], text: cap(blocks[at].text + text) };
  return { ...streaming, blocks };
}

function applyOutput(
  streaming: Streaming,
  kind: string,
  data: OutputData,
): Streaming {
  switch (kind) {
    case "assistant_delta":
      if ("index" in data && "text" in data) {
        return appendBlock(streaming, data.index, "text", data.text);
      }
      return streaming;
    case "thinking_delta":
      if ("index" in data && "text" in data) {
        return appendBlock(streaming, data.index, "thinking", data.text);
      }
      return streaming;
    case "tool_call_start":
      if ("index" in data && "call_id" in data && "name" in data) {
        return {
          ...streaming,
          blocks: [
            ...streaming.blocks.filter((b) => b.index !== data.index),
            {
              index: data.index,
              kind: "call" as const,
              text: "",
              callId: data.call_id,
              name: data.name,
            },
          ].sort((a, b) => a.index - b.index),
        };
      }
      return streaming;
    case "tool_call_input":
      if ("index" in data && "fragment" in data) {
        return appendBlock(streaming, data.index, "call", data.fragment);
      }
      return streaming;
    case "tool_output":
      if ("call_id" in data && "chunk" in data) {
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
