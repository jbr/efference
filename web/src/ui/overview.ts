// The session overview's model (docs/web-client.md): what one card says about a
// session, derived from its listing entry and — when one is open — its own
// `/state`. Pure, so the page holds no derivation and the rules are testable.
//
// Supervision is shown as a relation between peers, not as nesting: a
// supervised session gets its own card like any other, naming its supervisor,
// and a supervisor's card names what it supervises. Working with a subagent
// should be no further away than working with the session that created it.

import type { ArchivedSession, SessionEntry } from "../api/http.ts";
import type { BlockedReason, LogEntry, StateSnapshot } from "../api/types.ts";
import { callView } from "./signature.ts";
import { viewOf } from "../session/assistant.ts";
import { homeRelative } from "./identity.ts";

/** Where the operator's attention stands with a session, most urgent first.
 * `blocked`: it cannot move until someone acts (a gate, a stall, a park, a block).
 * `your_turn`: it finished a turn and is waiting on a reply or an archive.
 * `working`: mid-turn, nothing asked of anyone.
 * `new`: created and never sent a turn. */
export type Attention = "blocked" | "your_turn" | "working" | "new";

export interface CardModel {
  id: string;
  title: string | null;
  cwd: string;
  status: SessionEntry["status"];
  attention: Attention;
  processes: number;
  supervisor: string | null;
  supervises: string[];
  /** The one line of what is happening: the gate's ask, the stall's message,
   * the running call's intent, or the last reply. `null` when none is known. */
  activity: Activity | null;
  /** Why a `parked` session is parked: the cache still believed expired, or a
   * compaction that ended the belief; `null` when not known. */
  park: "expired" | "compacted" | null;
  /** Why a `blocked` session is blocked (the `loop_phase` topic); `null` when not known. */
  blocked: BlockedReason | null;
  context: { used: number | null; limit: number | null } | null;
  /** The provider's billed running total; `unpriced > 0` makes it a floor. */
  cost: { usd: number; unpriced: number } | null;
  /** `triage` is `null` when it is the main model — the default. */
  models: { main: string; spotter: string; triage: string | null } | null;
}

export type Activity =
  | { kind: "gate"; tool: string; detail: string }
  | { kind: "ask"; detail: string }
  | { kind: "stall"; detail: string }
  | { kind: "park"; detail: string }
  | { kind: "intent"; detail: string }
  | { kind: "reply"; detail: string };

/** What the log's recent entries say that `/state` does not: the latest tool
 * call's intent and the latest assistant prose. Read off a log tail per card
 * until the host serves them (backlog/inbox/host-overview-stream.md). */
export interface Recent {
  intent?: string;
  lastReply?: string;
}

/** Fold one log entry into what the card knows. Record-only assistant entries
 * (an aborted or failed stream) never reached the model's context, so they say
 * nothing about what the session is doing. A turn's first entry is the user's,
 * and it retires the previous reply: that reply is no longer what the session
 * is waiting on. */
export function foldRecent(recent: Recent, entry: LogEntry): Recent {
  const payload = entry.payload;
  if ("User" in payload) return { intent: recent.intent };
  if (!("Assistant" in payload) || payload.Assistant.excluded !== undefined) {
    return recent;
  }
  const view = viewOf(payload.Assistant.output);
  const next = { ...recent };
  const prose = view.text
    .map((block) => ("Text" in block ? block.Text : ""))
    .join("\n\n")
    .trim();
  if (prose !== "") next.lastReply = plainFirstParagraph(prose);
  for (const call of view.calls) {
    const intent = callView(call.name, call.input).intent;
    if (intent !== null) next.intent = intent;
  }
  return next;
}

/** A reply's first paragraph as plain text for a three-line preview: the
 * common inline markdown marks are dropped rather than rendered. */
export function plainFirstParagraph(markdown: string): string {
  const first = markdown.split(/\n\s*\n/)[0] ?? "";
  return first
    .replace(/^#+\s*/gm, "")
    .replace(/[*_`]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

/** A gate's input, as one line: the command for a shell call, otherwise the
 * first string argument, otherwise the JSON. */
export function gatePreview(input: unknown): string {
  if (input !== null && typeof input === "object") {
    const record = input as Record<string, unknown>;
    for (const key of ["command", "path", "url"]) {
      if (typeof record[key] === "string") return record[key];
    }
    const first = Object.values(record).find((v) => typeof v === "string");
    if (typeof first === "string") return first;
  }
  return JSON.stringify(input);
}

/** The predicate the card's top line completes after the session's id —
 * "amber-lattice needs approval" — or nothing: working is the default state and
 * needs no words, and a new session's title already says it. */
export function statusPhrase(card: CardModel): string | null {
  if (card.status === "starting") return "is starting";
  if (card.status === "gated") return "needs approval";
  if (card.status === "asking") return "has a question";
  if (card.status === "stalled") return "has stalled";
  if (card.status === "parked") {
    return card.park === "compacted"
      ? "is paused: summarized, waiting for you to continue"
      : "is paused: its cache expired";
  }
  if (card.status === "blocked") {
    return card.blocked === "pace_held"
      ? "is held"
      : card.blocked === "config_repair"
        ? "is stopped: its config file needs repair"
        : "is stopped until someone acts";
  }
  if (card.attention === "your_turn") return "is waiting for you";
  return null;
}

/** What continuing a session parked on an expired cache costs, in the card's
 * activity line. */
export function parkDetail(contextTokens: number | null): string {
  return contextTokens === null
    ? "continuing reads the whole context uncached"
    : `continuing reads ${tokens(contextTokens)} tokens uncached`;
}

export function cardModel(
  entry: SessionEntry,
  all: SessionEntry[],
  state: StateSnapshot | null,
  recent: Recent = {},
): CardModel {
  const usage = state?.usage ?? null;
  const lastActivityAt = usage?.last_request_at ?? null;
  const attention: Attention =
    entry.status === "gated" ||
    entry.status === "stalled" ||
    // Parked at the cold check, or blocked: the loop waits on a person, as at a gate.
    entry.status === "parked" ||
    entry.status === "blocked"
      ? "blocked"
      : // Being prepared is work the operator waits on, like a running turn.
        entry.status === "running" || entry.status === "starting"
        ? "working"
        : // A question is a turn ending, so it waits on the operator like one.
          entry.status === "asking"
          ? "your_turn"
          : lastActivityAt === null && entry.title === undefined
            ? "new"
            : "your_turn";

  const gate = state?.gate ?? null;
  const stall = state?.stall ?? null;
  const ask = state?.ask ?? null;
  // The park's reason: the `cold` topic still set is an expired cache, cleared is
  // a compaction that ended the belief while the loop waited.
  const cold = state?.cold;
  const park =
    entry.status !== "parked" || cold === undefined
      ? null
      : cold === null
        ? ({ reason: "compacted" } as const)
        : ({
            reason: "expired",
            context_tokens: cold.context_tokens,
          } as const);
  const phase = state?.loop_phase ?? null;
  const activity: Activity | null =
    gate !== null
      ? {
          kind: "gate",
          tool: gate.name,
          detail: gatePreview(gate.input),
        }
      : ask !== null
        ? { kind: "ask", detail: ask.question }
        : stall !== null
          ? { kind: "stall", detail: stall.message }
          : entry.status === "parked"
            ? {
                kind: "park",
                detail:
                  park?.reason === "compacted"
                    ? "send or continue to resume"
                    : parkDetail(
                        park?.reason === "expired" ? park.context_tokens : null,
                      ),
              }
            : entry.status === "running" && recent.intent !== undefined
              ? { kind: "intent", detail: recent.intent }
              : recent.lastReply !== undefined
                ? { kind: "reply", detail: recent.lastReply }
                : null;

  const inference = state?.session_identity?.inference;
  return {
    id: entry.id,
    title: entry.title ?? null,
    cwd: entry.cwd ?? "",
    status: entry.status,
    attention,
    processes: entry.processes,
    supervisor: entry.created_by ?? null,
    supervises: all
      .filter((other) => other.created_by === entry.id)
      .map((other) => other.id),
    activity,
    park: park?.reason ?? null,
    blocked:
      entry.status === "blocked" && phase?.phase === "blocked"
        ? phase.reason
        : null,
    context: usage?.context ?? null,
    cost: usage?.cost ?? null,
    models: inference
      ? {
          main: inference.main.model,
          spotter: inference.spotter.model,
          triage:
            inference.triage.shared ||
            inference.triage.model === inference.main.model
              ? null
              : inference.triage.model,
        }
      : null,
  };
}

export interface Workspace {
  cwd: string;
  /** The home-relative path up to and including its last `/` — `~/code/` —
   * set small, because it is what tells two same-named projects apart but is
   * not what the operator recognizes a workspace by. */
  parent: string;
  /** The last path component — `efference` — the part expected to be unique. */
  name: string;
  cards: CardModel[];
}

/** Split a home-relative path at its last `/`: `~/code/efference` →
 * `["~/code/", "efference"]`; `~` or `/` alone is all name. */
export function splitPath(path: string): [string, string] {
  const cut = path.lastIndexOf("/");
  if (cut <= 0 || cut === path.length - 1) return ["", path];
  return [path.slice(0, cut + 1), path.slice(cut + 1)];
}

/** Live workspaces keep path order and cards keep listing order. Up to five
 * archive-only workspaces follow in latest-activity order (docs/web-client.md). */
export function workspaces(
  cards: CardModel[],
  archivedByCwd: ReadonlyMap<string, ArchivedSession[]> = new Map(),
): Workspace[] {
  const groups = new Map<string, CardModel[]>();
  for (const card of cards) {
    const group = groups.get(card.cwd);
    if (group) group.push(card);
    else groups.set(card.cwd, [card]);
  }
  const recent = [...archivedByCwd.entries()]
    .filter(
      ([cwd, sessions]) => !groups.has(cwd) && sessions.some((s) => !s.problem),
    )
    .map(([cwd, sessions]) => ({
      cwd,
      lastActive: sessions.reduce(
        (latest, s) =>
          s.problem
            ? latest
            : Math.max(latest, s.last_active_at ?? s.created_at ?? 0),
        0,
      ),
    }))
    .sort((a, b) => b.lastActive - a.lastActive || a.cwd.localeCompare(b.cwd))
    .slice(0, 5);
  return [
    ...[...groups.entries()].sort(([a], [b]) => a.localeCompare(b)),
    ...recent.map(({ cwd }): [string, CardModel[]] => [cwd, []]),
  ].map(([cwd, cards]) => {
    const [parent, name] = splitPath(homeRelative(cwd));
    return { cwd, parent, name, cards };
  });
}

export const tokens = (n: number) =>
  n >= 1000 ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k` : `${n}`;
