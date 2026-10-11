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
    ResizeObserver: class {
      observe() {}
      disconnect() {}
    },
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
  expect(html).toContain("answering · question 1 of 2");
  expect(html).toContain('class="entry ask-question ask-question-current"');
  expect(html).toContain('class="entry ask-question ask-question-upcoming"');
  expect(html).not.toContain("▸");
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
  expect(html).toContain("answering · question 2 of 2");
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
  expect(html).toContain("answering<");
});

test("with no topic an open question is only asked", () => {
  const html = render([asking]);
  expect(html).toContain("asked · question 1 of 2");
  expect(html).not.toContain("answering ·");
  expect(html).not.toContain("next");
});

test("a restart is a notice, not an unknown entry", () => {
  const html = render([entry(7, { Resumed: { gap_ms: 303_060 } })]);
  expect(html).toContain(
    "session resumed after 5m: the host process restarted.",
  );
  expect(html).not.toContain("unknown entry");
});

test("a worktree event is a notice with its setup output disclosed", () => {
  const html = render([
    entry(7, {
      Worktree: {
        path: "/p/.efference/worktrees/quiet-fox",
        branch: "efference/quiet-fox",
        kind: {
          event: "added",
          base: "abc123",
          exit_code: 0,
          output:
            "Preparing worktree (new branch 'efference/quiet-fox')\nUpdating files:  96% (1438/1485)\rUpdating files: 100% (1485/1485), done.\n",
        },
      },
    }),
  ]);
  expect(html).toContain("worktree added on");
  expect(html).toContain(
    '<code class="worktree-branch">efference/quiet-fox</code>',
  );
  expect(html).not.toContain("`efference/quiet-fox`");
  expect(html).toContain("Show setup output");
  expect(html).toContain("worktree-setup");
  expect(html).toContain("Updating files: 100% (1485/1485), done.");
  expect(html).not.toContain("96% (1438/1485)");
  expect(html).not.toContain("unknown entry");
});

test("a worktree add with a failed setup states it on the line", () => {
  const html = render([
    entry(8, {
      Worktree: {
        path: "/p/.efference/worktrees/quiet-fox",
        branch: "efference/quiet-fox",
        kind: {
          event: "added",
          base: "abc123",
          exit_code: 1,
          output: "hook failed\n",
        },
      },
    }),
  ]);
  expect(html).toContain(
    '<code class="worktree-branch">efference/quiet-fox</code>',
  );
  expect(html).toContain("(setup failed: exit 1)");
  expect(html).toContain("hook failed");
  expect(html).not.toContain("unknown entry");
});

test("a worktree removal without setup output is a single line", () => {
  const html = render([
    entry(9, {
      Worktree: {
        path: "/p/.efference/worktrees/quiet-fox",
        branch: "efference/quiet-fox",
        kind: { event: "removed", branch_deleted: false },
      },
    }),
  ]);
  expect(html).toContain("worktree removed (branch kept)");
  expect(html).not.toContain("Show setup output");
  expect(html).not.toContain("unknown entry");
});
