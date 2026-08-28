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
  Process,
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
  /** Bytes the supervisor dropped at its retention cap before the seed read,
   * per process — reported rather than silently skipped (docs/supervisor.md). */
  processLost: Record<number, number>;
  /** True from an `Assistant` commit until the next log entry: `log` has strict
   * priority over `output` in `Tail::next_frame` (src/daemon/stream.rs), so the
   * committing entry can overtake deltas still queued that describe it. Those
   * arrive after `blocks` was cleared and would open fresh blocks holding the
   * tail of what just committed — a nameless call block from a trailing `}`, or
   * the message's last sentence a second time. The committed entry is
   * authoritative, so dropping them loses nothing. The next log entry
   * (a `ToolResult`, or a `User` turn) ends the drain; an assistant response
   * never follows another with no entry between. */
  draining: boolean;
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
   * each is keyed by the client-minted submission id the daemon echoes onto
   * the resulting `User` entry, so settling is an exact lookup rather than a
   * content match. */
  pendingTurns: PendingTurn[];
  /** Process id by the `tool_use` id that spawned it (docs/supervisor.md) —
   * accumulated from the `processes` topic and **never removed**.
   *
   * The topic carries only *running* processes, so an exited one leaves it; its
   * retained output stays readable at `GET /processes/:id/output`, and this is
   * what still knows the `:id` to ask for. A client that connects after the
   * process exited never saw the row and has no mapping — the registry is
   * ephemeral either way, so that gap is the daemon's, not a fold this could
   * fix. */
  processCalls: Record<CallId, number>;
}

/** One queued delivery echo: the submitted text (for display) under the
 * submission id its `User` entry will carry (docs/control-api.md). */
export interface PendingTurn {
  id: string;
  text: string;
}

export const emptyStreaming: Streaming = {
  blocks: [],
  toolOutput: {},
  processOutput: {},
  processLost: {},
  draining: false,
};

export const emptySession: Session = {
  connection: "connecting",
  entries: [],
  lastSeq: null,
  topics: {},
  streaming: emptyStreaming,
  pendingTurns: [],
  processCalls: {},
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
 * `User` entry carrying `id` arrives. The daemon commits the entry and
 * broadcasts its frame independently of writing the 202 body, so the entry
 * can fold *before* the submit response resolves — queueing then would strand
 * an echo nothing will ever settle, so a turn whose entry already folded is
 * not queued at all. */
export function pushPendingTurn(
  session: Session,
  id: string,
  text: string,
): Session {
  if (session.entries.some((e) => e.submission_id === id)) return session;
  return { ...session, pendingTurns: [...session.pendingTurns, { id, text }] };
}

/** Drop the delivery echoes a recall actually withdrew (docs/web-client.md). Only
 * the ids the daemon reported popping are dropped — one it had already dequeued
 * stays queued and settles the ordinary way, when its `User` entry arrives. */
export function removePendingTurns(session: Session, ids: string[]): Session {
  if (ids.length === 0) return session;
  const gone = new Set(ids);
  return {
    ...session,
    pendingTurns: session.pendingTurns.filter((p) => !gone.has(p.id)),
  };
}

const settlePendingTurns = (
  pending: PendingTurn[],
  entry: LogEntry,
): PendingTurn[] =>
  entry.submission_id === undefined
    ? pending
    : pending.filter((p) => p.id !== entry.submission_id);

/** Seed the transcript window from a `GET /log?tail=` read, settling any
 * pending turn whose `User` entry the window absorbed — a re-seed (retry
 * after login, a fresh catch-up read) arrives outside `applyLog`, and must
 * not strand a queued echo. */
export function seed(session: Session, entries: LogEntry[]): Session {
  const last = entries.at(-1);
  return {
    ...session,
    entries,
    lastSeq: last ? last.seq : session.lastSeq,
    pendingTurns: entries.reduce(settlePendingTurns, session.pendingTurns),
  };
}

/** Fold a `processes` topic value into the call→process mapping, keeping every
 * pair ever seen. A level-triggered replacement that drops an exited process
 * must not drop what it taught us. */
function learnProcessCalls(
  known: Record<CallId, number>,
  data: unknown,
): Record<CallId, number> {
  if (!Array.isArray(data)) return known;
  let next = known;
  for (const proc of data as Process[]) {
    const callId = proc.call_id;
    if (typeof callId !== "string" || next[callId] === proc.id) continue;
    if (next === known) next = { ...known };
    next[callId] = proc.id;
  }
  return next;
}

export function applyFrame(session: Session, frame: Frame): Session {
  switch (frame.class) {
    case "log":
      return applyLog(session, frame.data);
    case "state":
      return {
        ...session,
        topics: { ...session.topics, [frame.kind]: frame.data },
        processCalls:
          frame.kind === "processes"
            ? learnProcessCalls(session.processCalls, frame.data)
            : session.processCalls,
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
    return { ...streaming, blocks: [], draining: true };
  }
  if ("ToolResult" in entry.payload) {
    const toolOutput = { ...streaming.toolOutput };
    delete toolOutput[entry.payload.ToolResult.call_id];
    return { ...streaming, toolOutput, draining: false };
  }
  return { ...streaming, draining: false };
}

/** Append to the block at `index`, opening it if this is its first delta. The
 * block list stays sorted by index, which is wire order. Deltas arriving while
 * `draining` describe a response that already committed and are dropped
 * (see `Streaming.draining`). */
function appendBlock(
  streaming: Streaming,
  index: number,
  kind: LiveBlock["kind"],
  text: string,
): Streaming {
  if (streaming.draining) return streaming;
  const at = streaming.blocks.findIndex((b) => b.index === index);
  if (at === -1) {
    // Only `tool_call_start` carries a call's id and name, so a fragment for an
    // index we never saw start could only open a nameless call block. The
    // committed entry holds the call's whole input; drop it.
    if (kind === "call") return streaming;
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
      if (streaming.draining) return streaming;
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

/**
 * Fold a process's retained output under the deltas already buffered for it.
 * The read is a snapshot taken when the request went out, and the caller marks
 * the id seeded *before* issuing it, so every delta in the buffer is strictly
 * later than the seed — prepending is what puts them back in order. A second
 * seed for an id is therefore never wanted, and this is a no-op if the read
 * came back empty.
 */
export function seedProcessOutput(
  session: Session,
  id: number,
  output: string,
  lost: number,
): Session {
  if (output === "" && lost === 0) return session;
  const streaming = session.streaming;
  return {
    ...session,
    streaming: {
      ...streaming,
      processOutput: {
        ...streaming.processOutput,
        [id]: cap(output + (streaming.processOutput[id] ?? "")),
      },
      processLost:
        lost === 0
          ? streaming.processLost
          : { ...streaming.processLost, [id]: lost },
    },
  };
}

/**
 * When a batch of folds becomes one notification. The default asks for an
 * animation frame, falling back to a timer where there is none (the suite's
 * node environment, a worker). A frame is the right unit because a
 * notification's only consumer is a render, and a render's only product is a
 * paint: more than one per frame is work whose result is overwritten before it
 * is seen.
 */
export type Scheduler = (flush: () => void) => void;

const frameScheduler: Scheduler = (flush) => {
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(flush);
  else setTimeout(flush, 16);
};

/** A subscribable holder for the fold — the seam useSyncExternalStore binds to.
 *
 * Folding and notifying are separate steps, and only the notify is batched
 * (docs/web-client.md, "Coalescing the fold"). The fold stays synchronous, so
 * `snapshot()` is current the instant `update` returns and every caller that
 * reads it back — the connector's resume cursor, the scrubber's refold, the
 * process seeder — is unaffected. What batching removes is the render per SSE
 * frame: a long block arrives as thousands of deltas, each of which re-parsed
 * the whole growing block as markdown, and that cost is linear in block length,
 * so the total is quadratic. Past the point where a delta costs more than a
 * delta's arrival interval, the main thread never catches up and the deltas
 * still buffered in the fetch stream keep it saturated after the stream itself
 * has gone quiet — which is why the page did not recover on its own. Coalescing
 * collapses a backlog of any size into one render at the current length, so the
 * outstanding work is bounded by the frame rate rather than by the delta count.
 */
export class Store {
  private session: Session = emptySession;
  private listeners = new Set<() => void>();
  /** A notification is owed: at least one fold landed since the last flush. */
  private pending = false;

  constructor(private readonly schedule: Scheduler = frameScheduler) {}

  snapshot = (): Session => this.session;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  update(f: (session: Session) => Session): void {
    const next = f(this.session);
    if (next === this.session) return;
    this.session = next;
    if (this.pending) return;
    this.pending = true;
    this.schedule(this.flush);
  }

  /** Notify now if anything is owed. The scheduled flush calls this; so does
   * anything that needs the render to have happened before it continues — the
   * scrubber stepping one frame at a time, and tests. */
  flush = (): void => {
    if (!this.pending) return;
    this.pending = false;
    for (const listener of this.listeners) listener();
  };
}
