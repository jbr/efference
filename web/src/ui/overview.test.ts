// The overview's attention rule, supervision as a peer relation, and grouping
// by working directory (docs/web-client.md).

import type { ArchivedSession } from "../api/types.ts";
import { expect, test } from "vitest";
import type { ListedSession } from "../api/types.ts";
import type { LogEntry, Payload } from "../api/entry.ts";
import type { StateSnapshot } from "../api/frames.ts";
import { topics } from "../api/samples/topics.ts";
import { messagesAssistant } from "../session/assistant.ts";
import {
  cardModel,
  foldRecent,
  gatePreview,
  plainFirstParagraph,
  splitPath,
  statusPhrase,
  workspaces,
} from "./overview.ts";

const entry = (over: Partial<ListedSession> = {}): ListedSession => ({
  id: "alpha",
  cwd: "/Users/x/p",
  status: "idle",
  processes: 0,
  ...over,
});

const asked: StateSnapshot = {
  last_seq: 3,
  usage: { ...topics.state[0].usage, last_request_at: 1000 },
};

test("idle is the operator's turn once a turn has run, and new before", () => {
  const one = [entry()];
  expect(cardModel(entry(), one, null).attention).toBe("new");
  expect(cardModel(entry(), one, asked).attention).toBe("your_turn");
  expect(cardModel(entry({ title: "t" }), one, null).attention).toBe(
    "your_turn",
  );
  expect(cardModel(entry({ status: "gated" }), one, null).attention).toBe(
    "blocked",
  );
  expect(cardModel(entry({ status: "stalled" }), one, null).attention).toBe(
    "blocked",
  );
  expect(cardModel(entry({ status: "running" }), one, null).attention).toBe(
    "working",
  );
});

test("a parked session is blocked, says why, and names the cold read", () => {
  const one = [entry({ status: "parked" })];
  const cold = { since: 1, context_tokens: 120_000 };
  const card = cardModel(one[0], one, {
    last_seq: null,
    loop_phase: { phase: "cold_parked" },
    cold,
  });
  expect(card.attention).toBe("blocked");
  expect(statusPhrase(card)).toBe("is paused: its cache expired");
  expect(card.activity).toEqual({
    kind: "park",
    detail: "continuing reads 120k tokens uncached",
  });
  expect(cardModel(one[0], one, null).activity).toEqual({
    kind: "park",
    detail: "continuing reads the whole context uncached",
  });
  const folded = cardModel(one[0], one, {
    last_seq: null,
    loop_phase: { phase: "cold_parked" },
    cold: null,
  });
  expect(folded.attention).toBe("blocked");
  expect(statusPhrase(folded)).toBe(
    "is paused: summarized, waiting for you to continue",
  );
});

test("a blocked session is blocked and says why", () => {
  const one = [entry({ status: "blocked" })];
  const held = cardModel(one[0], one, {
    last_seq: null,
    loop_phase: { phase: "blocked", reason: "pace_held" },
  });
  expect(held.attention).toBe("blocked");
  expect(statusPhrase(held)).toBe("is held");
  const repair = cardModel(one[0], one, {
    last_seq: null,
    loop_phase: { phase: "blocked", reason: "config_repair" },
  });
  expect(statusPhrase(repair)).toBe("is stopped: its config file needs repair");
  expect(statusPhrase(cardModel(one[0], one, null))).toBe(
    "is stopped until someone acts",
  );
});

test("a pending question is the operator's turn, and the card shows it", () => {
  const one = [entry()];
  const card = cardModel(entry({ status: "asking" }), one, {
    last_seq: null,
    ask: {
      call_id: "toolu_a",
      question: "Ship it?",
      n: 1,
      of: 1,
      recalled: [],
    },
  });
  expect(card.attention).toBe("your_turn");
  expect(card.activity).toEqual({ kind: "ask", detail: "Ship it?" });
  expect(cardModel(entry({ status: "asking" }), one, null).attention).toBe(
    "your_turn",
  );
});

test("supervision is named from both ends, and nothing is nested", () => {
  const all = [
    entry({ id: "boss" }),
    entry({ id: "a", created_by: "boss" }),
    entry({ id: "b", created_by: "boss" }),
  ];
  const cards = all.map((e) => cardModel(e, all, null));
  expect(cards.map((c) => c.id)).toEqual(["boss", "a", "b"]);
  expect(cards[0].supervises).toEqual(["a", "b"]);
  expect(cards[1].supervisor).toBe("boss");
});

test("a gate's activity is its command, before anything else", () => {
  const gated: StateSnapshot = {
    last_seq: 1,
    gate: { call_id: "c", name: "bash", input: { command: "rm -rf x" } },
  };
  expect(
    cardModel(entry({ status: "gated" }), [], gated, { lastReply: "r" })
      .activity,
  ).toEqual({ kind: "gate", tool: "bash", detail: "rm -rf x" });
  expect(gatePreview({ n: 1 })).toBe('{"n":1}');
});

test("workspaces group by cwd in path order and keep listing order within", () => {
  const all = [
    entry({ id: "z", cwd: "/Users/x/b" }),
    entry({ id: "y", cwd: "/Users/x/a" }),
    entry({ id: "x", cwd: "/Users/x/b" }),
  ];
  const groups = workspaces(all.map((e) => cardModel(e, all, null)));
  expect(groups.map((g) => g.name)).toEqual(["a", "b"]);
  expect(groups[1].cards.map((c) => c.id)).toEqual(["z", "x"]);
});

test("a workspace path splits into its parent and its name", () => {
  expect(splitPath("~/code/efference")).toEqual(["~/code/", "efference"]);
  expect(splitPath("~")).toEqual(["", "~"]);
  expect(splitPath("/")).toEqual(["", "/"]);
  expect(splitPath("/srv")).toEqual(["", "/srv"]);
  expect(splitPath("/srv/app")).toEqual(["/srv/", "app"]);
});

const logged = (seq: number, payload: Payload): LogEntry => ({
  seq,
  turn: 1,
  at: 0,
  payload,
});

const said = (seq: number, text: string, intent?: string) =>
  logged(seq, {
    Assistant: messagesAssistant(
      [],
      text === "" ? [] : [{ Text: text }],
      intent === undefined
        ? []
        : [{ id: `c${seq}`, name: "bash", input: { command: "ls", intent } }],
    ),
  });

test("the log tail yields the latest intent and the latest reply", () => {
  const recent = [
    said(1, "", "List the tree"),
    said(2, "Looking at the **store**.", "Read store.ts"),
    said(3, "Done.\n\nMore detail below."),
  ].reduce(foldRecent, {});
  expect(recent).toEqual({ intent: "Read store.ts", lastReply: "Done." });
});

test("a user turn retires the reply it answers; a failed stream says nothing", () => {
  const afterUser = [
    said(1, "Want me to rerun?", "Run it"),
    logged(2, { User: [{ Text: "yes" }] }),
  ].reduce(foldRecent, {});
  expect(afterUser).toEqual({ intent: "Run it" });

  const failed = logged(3, {
    Assistant: {
      ...messagesAssistant([], [{ Text: "partial" }], []),
      excluded: { Upstream: "reset" },
    },
  });
  expect(foldRecent({ lastReply: "kept" }, failed)).toEqual({
    lastReply: "kept",
  });
});

test("a reply preview is its first paragraph without inline marks", () => {
  expect(
    plainFirstParagraph("## Found it\nThe `timer` is **re-armed**.\n\nx"),
  ).toBe("Found it The timer is re-armed.");
});

test("recent archive-only workspaces follow every live workspace, capped at five", () => {
  const live = Array.from({ length: 6 }, (_, i) =>
    entry({ id: `live-${i}`, cwd: `/live/${i}` }),
  );
  const archives = new Map<string, ArchivedSession[]>();
  for (let i = 0; i < 7; i++) {
    archives.set(`/old/${i}`, [
      {
        id: `old-${i}`,
        cwd: `/old/${i}`,
        state: "archived",
        last_active_at: i + 1,
      },
    ]);
  }
  archives.set("/live/0", [
    { id: "older", cwd: "/live/0", state: "archived", last_active_at: 100 },
  ]);
  const groups = workspaces(
    live.map((e) => cardModel(e, live, null)),
    archives,
  );
  expect(groups.map((g) => g.cwd)).toEqual([
    ...live.map((e) => e.cwd),
    "/old/6",
    "/old/5",
    "/old/4",
    "/old/3",
    "/old/2",
  ]);
  expect(groups.slice(6).every((g) => g.cards.length === 0)).toBe(true);
});

test("archive recency uses the latest resumable session with deterministic ties", () => {
  const archive = (
    cwd: string,
    over: Partial<ArchivedSession> = {},
  ): ArchivedSession => ({ id: cwd, cwd, state: "archived", ...over });
  const archives = new Map([
    ["/b", [archive("/b", { last_active_at: 20 })]],
    [
      "/a",
      [
        archive("/a", { last_active_at: 1 }),
        archive("/a", { last_active_at: 20 }),
        archive("/a", { last_active_at: 100, problem: "missing log" }),
      ],
    ],
    ["/broken", [archive("/broken", { problem: "missing log" })]],
    ["/empty", []],
    ["/unknown", [archive("/unknown")]],
    ["/created", [archive("/created", { created_at: 10 })]],
  ]);
  expect(workspaces([], archives).map((g) => g.cwd)).toEqual([
    "/a",
    "/b",
    "/created",
    "/unknown",
  ]);
});

test("a workspace remains after its final live session is archived", () => {
  const session = entry();
  const archives = new Map<string, ArchivedSession[]>([
    [
      session.cwd!,
      [
        {
          id: session.id,
          cwd: session.cwd!,
          state: "archived",
          last_active_at: 1,
        },
      ],
    ],
  ]);
  const before = workspaces([cardModel(session, [session], null)], archives);
  const after = workspaces([], archives);
  expect(after.map((g) => g.cwd)).toEqual(before.map((g) => g.cwd));
  expect(after[0].cards).toEqual([]);
});
