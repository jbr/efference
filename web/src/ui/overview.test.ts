// The overview's attention rule, supervision as a peer relation, and grouping
// by working directory (docs/web-client.md).

import { expect, test } from "vitest";
import type { SessionEntry } from "../api/http.ts";
import type { LogEntry, Payload, StateSnapshot } from "../api/types.ts";
import { messagesAssistant } from "../session/assistant.ts";
import {
  cardModel,
  foldRecent,
  gatePreview,
  plainFirstParagraph,
  splitPath,
  workspaces,
} from "./overview.ts";

const entry = (over: Partial<SessionEntry> = {}): SessionEntry => ({
  id: "alpha",
  cwd: "/Users/x/p",
  status: "idle",
  processes: 0,
  ...over,
});

const asked: StateSnapshot = {
  last_seq: 3,
  usage: {
    last: null,
    cumulative: {
      input_tokens: 0,
      output_tokens: 0,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    },
    last_request_at: 1000,
    context: null,
    cost: null,
    plan: null,
  },
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
      upstream_failure: "reset",
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
