// @vitest-environment jsdom
// How an `ask` reads in the transcript (docs/web-client.md): the question as the
// assistant's prose with a mark, the reply as the operator's message, and the echo
// of a reply that is not queued. Rendered into jsdom.

import { expect, test, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Ask, LogEntry } from "../api/types.ts";
import { messagesAssistant } from "../session/assistant.ts";
import { emptyStreaming } from "../session/store.ts";
import { buildTree } from "../session/tree.ts";
import { Transcript } from "./Transcript.tsx";

// `useNarrow` reads a media query at module load; the static render has no window.
vi.hoisted(() => {
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
  });
  Object.defineProperty(window, "scrollTo", { value: () => {} });
  Object.defineProperty(window, "matchMedia", {
    value: () => ({
      matches: false,
      addEventListener: () => {},
      removeEventListener: () => {},
    }),
  });
});

const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 1754300000000,
  payload,
});

const ask = (id: string, question: string) => ({
  id,
  name: "ask",
  input: { question },
});

function markup(element: React.ReactElement): string {
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() => root.render(element));
  const html = host.innerHTML;
  act(() => root.unmount());
  return html;
}

const render = (
  entries: LogEntry[],
  over: { ask?: Ask | null; pending?: string[] } = {},
) =>
  markup(
    <Transcript
      nodes={buildTree(entries)}
      streaming={emptyStreaming}
      pending={(over.pending ?? []).map((text, i) => ({
        id: `p${i}`,
        text,
        attachments: [],
      }))}
      ask={over.ask ?? null}
      gatedCall={null}
      onGateSlot={() => {}}
      onRecall={() => {}}
      onInspect={() => {}}
      onOpenFile={() => {}}
    />,
  );

const topic = (call_id: string, n: number, of: number): Ask => ({
  call_id,
  question: "?",
  n,
  of,
  recalled: [],
});

const asking = entry(1, {
  Assistant: messagesAssistant(
    [],
    [],
    [ask("a", "Keep the legacy spelling?"), ask("b", "And the old flag?")],
  ),
});

test("a question is prose with an asked mark, not a call", () => {
  const html = render([asking]);
  expect(html).toContain("Keep the legacy spelling?");
  expect(html).toContain("asked · question 1 of 2");
  expect(html).toContain("asked · question 2 of 2");
  expect(html).not.toContain("call-head");
});

test("a single question carries a bare mark", () => {
  const html = render([
    entry(1, {
      Assistant: messagesAssistant([], [], [ask("a", "Ship it?")]),
    }),
  ]);
  expect(html).toContain(">asked<");
});

test("a reply is the operator's message and an unanswered ask says so", () => {
  const result = (seq: number, id: string, submission?: string): LogEntry => ({
    ...entry(seq, {
      ToolResult: {
        call_id: id,
        raw: { content: "yes, keep it", is_error: submission === undefined },
        wire: "Raw",
      },
    }),
    ...(submission === undefined ? {} : { submission_id: submission }),
  });
  const html = render([asking, result(2, "a", "s1"), result(3, "b")]);
  expect(html).toContain("yes, keep it");
  expect(html).toContain('class="entry user"');
  expect(html).toContain("not answered");
});

test("the echo of a reply says answering and offers no recall", () => {
  const queued = render([asking], { pending: ["yes"] });
  expect(queued).toContain("Queued");
  expect(queued).toContain("Recall");
  const answering = render([asking], {
    pending: ["yes"],
    ask: topic("a", 1, 2),
  });
  expect(answering).toContain("Sent · answering the question");
  expect(answering).not.toContain("Recall");
});

test("the topic names the current question and the next one is marked upcoming", () => {
  const html = render([asking], { ask: topic("a", 1, 2) });
  expect(html).toContain("▸ answering · question 1 of 2");
  expect(html).toContain("next · question 2 of 2");
  expect(html).not.toContain("asked · question");
});

test("an answered question reads as asked while the next is current", () => {
  const first: LogEntry = {
    ...entry(2, {
      ToolResult: {
        call_id: "a",
        raw: { content: "one", is_error: false },
        wire: "Raw",
      },
    }),
    submission_id: "s1",
  };
  const html = render([asking, first], { ask: topic("b", 2, 2) });
  expect(html).toContain("asked · question 1 of 2");
  expect(html).toContain("▸ answering · question 2 of 2");
  expect(html).not.toContain("next");
});

test("a lone current question has no position", () => {
  const html = render(
    [
      entry(1, {
        Assistant: messagesAssistant([], [], [ask("a", "Ship it?")]),
      }),
    ],
    { ask: topic("a", 1, 1) },
  );
  expect(html).toContain("▸ answering<");
});

test("with no topic an open question is only asked", () => {
  const html = render([asking]);
  expect(html).toContain("asked · question 1 of 2");
  expect(html).not.toContain("answering ·");
  expect(html).not.toContain("next");
});
