// The client-side session model: a pure fold of control-stream frames over an
// immutable snapshot (docs/control-api.md — `GET /state` ≡ the fold of every
// `state` frame; the log is append-only; `output` is a lossy pre-commit tail).
// Pure functions so the fold is testable without a browser or a daemon;
// Store wraps them with subscription for useSyncExternalStore. Nothing here
// imports React or touches the network.

import type {
  AskView,
  Arrival,
  CallId,
  ProcessState,
  Spot,
  StreamDelta,
} from "../api/types.ts";
import type { Frame, TopicValues } from "../api/frames.ts";
import type { LogEntry, Seq } from "../api/entry.ts";
import {
  closeSide,
  emptySideStreaming,
  sideLog,
  sideOperations,
  sideOutput,
  type SideStreaming,
} from "./side-stream.ts";

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
export interface Streaming extends SideStreaming {
  /** The in-flight response's blocks, in wire order. */
  blocks: LiveBlock[];
  /** Live tool output by call id, cleared when the result commits. */
  toolOutput: Record<CallId, string>;
  processOutput: Record<number, string>;
  /** Bytes the process monitor dropped at its retention cap before the seed read,
   * per process — reported rather than silently skipped (docs/process-monitor.md). */
  processLost: Record<number, number>;
  /** True from an `Assistant` commit until the next log entry: `log` has strict
   * priority over `output` in `Tail::next_frame` (src/routes/stream.rs), so the
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
  | "connecting"
  | "live"
  | "reconnecting"
  | "missing"
  // The stream ended after a failed start: nothing is left to reconnect to
  // (docs/control-api.md).
  | "ended";

export interface Session {
  connection: Connection;
  /** Committed entries in seq order — the window we've seen, not the whole log. */
  entries: LogEntry[];
  lastSeq: Seq | null;
  topics: TopicValues;
  streaming: Streaming;
  /** Turns the daemon accepted (a 202) whose `User` entries have not streamed
   * back yet — the operator's delivery signal (docs/web-client.md). Ordered;
   * each is keyed by the client-minted submission id the daemon echoes onto
   * the resulting `User` entry, so settling is an exact lookup rather than a
   * content match. */
  pendingTurns: PendingTurn[];
  /** Submission ids this client minted and has not yet seen settle, recorded
   * **before** the POST so an `ask`'s recalled turns can be recognised as ours
   * even while a 202 is still in flight (docs/web-client.md). A restarted
   * client has none, so it restores nothing. */
  minted: string[];
  /** Ids an `ask` handed back to the composer: their 202 may still resolve, and
   * an echo pushed then would never settle. */
  withdrawn: string[];
  /** The `call_id` of the last `ask` whose recalled turns were restored, so a
   * re-delivered topic frame restores them once. */
  askRestored: CallId | null;
  /** What that restore put back, for the composer to splice; `nonce` makes a
   * second restore a new event. */
  askRecall: AskRecall | null;
  /** Process id by the `tool_use` id that spawned it (docs/process-monitor.md) —
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
  attachments: Arrival[];
}

/** The turns an `ask` handed back to the composer. */
export interface AskRecall {
  turns: NonNullable<TopicValues["ask"]>["recalled"];
  nonce: number;
}

export const emptyStreaming: Streaming = {
  ...emptySideStreaming,
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
  minted: [],
  withdrawn: [],
  askRestored: null,
  askRecall: null,
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
  attachments: Arrival[] = [],
): Session {
  if (session.entries.some((e) => e.submission_id === id)) return session;
  if (session.withdrawn.includes(id)) return session;
  return {
    ...session,
    pendingTurns: [...session.pendingTurns, { id, text, attachments }],
  };
}

/** Record a submission id as this client's, before its POST goes out. */
export function mintTurn(session: Session, id: string): Session {
  return { ...session, minted: [...session.minted, id] };
}

/** Forget a minted id whose POST failed: nothing was submitted under it. */
export function unmintTurn(session: Session, id: string): Session {
  return { ...session, minted: session.minted.filter((m) => m !== id) };
}

/** Take back the turns an `ask` withdrew from the queue (docs/web-client.md), once
 * per ask: only those whose ids this client minted, whose echoes go with them.
 * They reach the composer through `askRecall`. */
function restoreAskRecall(session: Session, ask: AskView | null): Session {
  if (ask === null || ask.call_id === session.askRestored) return session;
  const own = ask.recalled.filter((t) =>
    session.minted.includes(t.submission_id),
  );
  const next: Session = { ...session, askRestored: ask.call_id };
  if (own.length === 0) return next;
  const ids = own.map((t) => t.submission_id);
  return {
    ...next,
    minted: next.minted.filter((id) => !ids.includes(id)),
    withdrawn: [...next.withdrawn, ...ids],
    pendingTurns: next.pendingTurns.filter((p) => !ids.includes(p.id)),
    askRecall: { turns: own, nonce: (session.askRecall?.nonce ?? 0) + 1 },
  };
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

const settleMinted = (minted: string[], entry: LogEntry): string[] =>
  entry.submission_id === undefined
    ? minted
    : minted.filter((id) => id !== entry.submission_id);

const settlePendingTurns = (
  pending: PendingTurn[],
  entry: LogEntry,
): PendingTurn[] =>
  entry.submission_id === undefined
    ? pending
    : pending.filter((p) => p.id !== entry.submission_id);

/** Catch up a session that already holds entries, from a fresh `GET /log?tail=`
 * read: when the read reaches back to the cursor, the entries held below it
 * are kept and the read replaces the rest, so a warm store resuming does not
 * lose what it had scrolled through. A read that does not reach (the gap is
 * longer than the window) is a plain re-seed. */
export function reseed(session: Session, tail: LogEntry[]): Session {
  const first = tail[0];
  if (first === undefined) return session;
  if (session.lastSeq === null || first.seq > session.lastSeq + 1) {
    return seed(session, tail);
  }
  const kept = session.entries.filter((entry) => entry.seq < first.seq);
  return seed(session, [...kept, ...tail]);
}

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
    streaming: entries.reduce(
      (streaming, entry) => sideLog(streaming, entry, entries),
      session.streaming,
    ),
    pendingTurns: entries.reduce(settlePendingTurns, session.pendingTurns),
    minted: entries.reduce(settleMinted, session.minted),
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
  for (const proc of data as ProcessState[]) {
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
      return restoreAskRecall(
        {
          ...session,
          topics: { ...session.topics, [frame.kind]: frame.data },
          streaming:
            frame.kind === "current_operations"
              ? sideOperations(
                  session.streaming,
                  (frame.data as TopicValues["current_operations"]) ?? null,
                )
              : frame.kind === "compaction_failure" && frame.data !== null
                ? closeSide(session.streaming, "compaction")
                : session.streaming,
          processCalls:
            frame.kind === "processes"
              ? learnProcessCalls(session.processCalls, frame.data)
              : session.processCalls,
        },
        frame.kind === "ask" ? (frame.data as AskView | null) : null,
      );
    case "output":
      return {
        ...session,
        streaming: applyOutput(sideOutput(session.streaming, frame), frame),
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
    streaming: settle(
      sideLog(session.streaming, entry, session.entries),
      entry,
    ),
    pendingTurns: settlePendingTurns(session.pendingTurns, entry),
    minted: settleMinted(session.minted, entry),
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

function applyOutput(streaming: Streaming, delta: StreamDelta): Streaming {
  switch (delta.kind) {
    case "assistant_delta":
      return appendBlock(streaming, delta.data.index, "text", delta.data.text);
    case "thinking_delta":
      return appendBlock(
        streaming,
        delta.data.index,
        "thinking",
        delta.data.text,
      );
    case "tool_call_start": {
      if (streaming.draining) return streaming;
      const { index, call_id, name } = delta.data;
      return {
        ...streaming,
        blocks: [
          ...streaming.blocks.filter((b) => b.index !== index),
          {
            index,
            kind: "call" as const,
            text: "",
            callId: call_id,
            name,
          },
        ].sort((a, b) => a.index - b.index),
      };
    }
    case "tool_call_input":
      return appendBlock(
        streaming,
        delta.data.index,
        "call",
        delta.data.fragment,
      );
    case "tool_output":
      return {
        ...streaming,
        toolOutput: {
          ...streaming.toolOutput,
          [delta.data.call_id]: cap(
            (streaming.toolOutput[delta.data.call_id] ?? "") + delta.data.chunk,
          ),
        },
      };
    case "process_output":
      return {
        ...streaming,
        processOutput: {
          ...streaming.processOutput,
          [delta.data.id]: cap(
            (streaming.processOutput[delta.data.id] ?? "") + delta.data.chunk,
          ),
        },
      };
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

/** A subscribable holder for the fold — the interface useSyncExternalStore binds to.
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
