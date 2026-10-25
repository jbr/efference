// @vitest-environment jsdom
// How an `ask` reads in the transcript (docs/web-client.md): the question as the
// assistant's prose with a mark, the reply as the operator's message, and the echo
// of a reply that is not queued. Rendered into jsdom.

import { afterEach, expect, test, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Ask, LogEntry } from "../api/types.ts";
import { messagesAssistant } from "../session/assistant.ts";
import { emptyStreaming } from "../session/store.ts";
import type { Streaming } from "../session/store.ts";
import { buildTree } from "../session/tree.ts";
import { Transcript } from "./Transcript.tsx";

// `useNarrow` reads a media query at module load; the static render has no window.
vi.hoisted(() => {
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
    ResizeObserver: class {
      observe() {}
      unobserve() {}
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

async function markupAfterEffects(
  element: React.ReactElement,
): Promise<string> {
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () => {
    root.render(element);
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  const html = host.innerHTML;
  act(() => root.unmount());
  return html;
}

const element = (
  entries: LogEntry[],
  over: {
    ask?: Ask | null;
    pending?: string[];
    streaming?: Streaming;
    artifactEnabled?: boolean;
  } = {},
) => (
  <Transcript
    nodes={buildTree(entries)}
    streaming={over.streaming ?? emptyStreaming}
    pending={(over.pending ?? []).map((text, i) => ({
      id: `p${i}`,
      text,
      attachments: [],
    }))}
    ask={over.ask ?? null}
    gatedCall={null}
    artifactEnabled={over.artifactEnabled ?? false}
    onGateSlot={() => {}}
    onRecall={() => {}}
    onInspect={() => {}}
    onOpenFile={() => {}}
  />
);

const render = (entries: LogEntry[], over?: Parameters<typeof element>[1]) =>
  markup(element(entries, over));

const renderWithArtifacts = (
  entries: LogEntry[],
  over: Parameters<typeof element>[1] = {},
) => markupAfterEffects(element(entries, { ...over, artifactEnabled: true }));

afterEach(() => vi.unstubAllGlobals());

function stubArtifactResolution(status = 200) {
  const body =
    status === 200
      ? { url: "/artifacts/test-token/" }
      : { code: "not_found", message: "artifact unavailable" };
  const fetchMock = vi.fn<typeof fetch>(() =>
    Promise.resolve(new Response(JSON.stringify(body), { status })),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

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

test("a closed html-artifact fence resolves to a sandboxed preview with source access", async () => {
  const fetchMock = stubArtifactResolution();
  const html = await renderWithArtifacts([
    entry(20, {
      Assistant: messagesAssistant(
        [],
        [{ Text: "```html-artifact\n<div>preview content</div>\n```\n" }],
        [],
      ),
    }),
  ]);

  expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
    "/v1/artifacts/20/0/0",
  ]);
  expect(html).toContain('src="/artifacts/test-token/"');
  expect(html).toContain('sandbox="allow-scripts"');
  expect(html).not.toContain("allow-same-origin");
  expect(html).toContain("Source");
  expect(html).toContain("Copy source");
  expect(html).toContain("preview content");
});

// SessionView rebuilds the transcript tree on every streamed token. A committed
// entry's frame that remounted then would reload and resolve on each one.
test("a streaming delta leaves committed artifact frames and code blocks mounted", async () => {
  const fetchMock = stubArtifactResolution();
  const entries = [
    entry(25, {
      Assistant: messagesAssistant(
        [],
        [
          {
            Text: "```html-artifact\n<div>chart</div>\n```\n\n```rust\nfn main() {}\n```\n",
          },
        ],
        [],
      ),
    }),
  ];
  const host = document.createElement("div");
  const root = createRoot(host);
  const settle = (element: React.ReactElement) =>
    act(async () => {
      root.render(element);
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  await settle(element(entries, { artifactEnabled: true }));
  const frame = host.querySelector("iframe");
  const code = host.querySelector('pre[data-lang="rust"]');
  expect(frame).not.toBeNull();
  expect(code).not.toBeNull();

  await settle(
    element(entries, {
      artifactEnabled: true,
      streaming: {
        ...emptyStreaming,
        blocks: [{ index: 0, kind: "text", text: "the next token" }],
      },
    }),
  );

  expect(host.querySelector("iframe")).toBe(frame);
  expect(host.querySelector('pre[data-lang="rust"]')).toBe(code);
  expect(fetchMock).toHaveBeenCalledTimes(1);
  act(() => root.unmount());
});

test.each([
  ["ordinary HTML", "```html\n<div>ordinary HTML</div>\n```", "ordinary HTML"],
  ["ordinary SVG", "```svg\n<svg>ordinary SVG</svg>\n```", "ordinary SVG"],
  ["an unlabelled fence", "```\nplain code\n```", "plain code"],
  [
    "an indented fence-like block",
    "    ```html-artifact\nnot a fence\n    ```",
    "not a fence",
  ],
  [
    "html-artifact with extra info",
    "```html-artifact preview\n<div>source only</div>\n```",
    "source only",
  ],
])(
  "%s remains source rather than an artifact",
  async (_label, source, expected) => {
    const fetchMock = stubArtifactResolution();
    const html = await renderWithArtifacts([
      entry(21, {
        Assistant: messagesAssistant([], [{ Text: source }], []),
      }),
    ]);

    expect(fetchMock).not.toHaveBeenCalled();
    expect(html).not.toContain("<iframe");
    expect(html).toContain(expected);
  },
);

test("an unclosed html-artifact fence stays source", async () => {
  const fetchMock = stubArtifactResolution();
  const html = await renderWithArtifacts([
    entry(22, {
      Assistant: messagesAssistant(
        [],
        [{ Text: "```html-artifact\n<div>unfinished artifact</div>\n" }],
        [],
      ),
    }),
  ]);

  expect(fetchMock).not.toHaveBeenCalled();
  expect(html).not.toContain("<iframe");
  expect(html).toContain("unfinished artifact");
});

test.each([
  ["blockquote", "> ```"],
  ["list marker", "- ```"],
  ["list-derived indentation", "- source item\n    ```"],
])(
  "a %s-looking code line cannot close a top-level artifact",
  async (_container, line) => {
    const fetchMock = stubArtifactResolution();
    const html = await renderWithArtifacts([
      entry(22, {
        Assistant: messagesAssistant(
          [],
          [
            {
              Text: `\`\`\`html-artifact\n<div>unfinished container content</div>\n${line}`,
            },
          ],
          [],
        ),
      }),
    ]);

    expect(fetchMock).not.toHaveBeenCalled();
    expect(html).not.toContain("<iframe");
    expect(html).toContain("unfinished container content");
  },
);

test("fence ordinals include other fences and restart for each assistant Text block", async () => {
  const fetchMock = stubArtifactResolution();
  const html = await renderWithArtifacts([
    entry(23, {
      Assistant: messagesAssistant(
        [],
        [
          {
            Text: "```javascript\nconst before = true;\n```\n```html-artifact\n<div>first</div>\n```\n",
          },
          { Text: "```html-artifact\n<div>second</div>\n```\n" },
        ],
        [],
      ),
    }),
  ]);

  expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
    "/v1/artifacts/23/0/1",
    "/v1/artifacts/23/1/0",
  ]);
  expect((html.match(/<iframe/g) ?? []).length).toBe(2);
});

test("fence ordinals include nested blockquote and list fences", async () => {
  const fetchMock = stubArtifactResolution();
  await renderWithArtifacts([
    entry(24, {
      Assistant: messagesAssistant(
        [],
        [
          {
            Text: [
              "> ```javascript",
              "> const before = true;",
              "> ```",
              ">",
              "> ```html-artifact",
              "> <div>nested preview</div>",
              "> ```",
              "",
              "- list item",
              "  ```html-artifact",
              "  <div>list preview</div>",
              "  ```",
              "",
              "  - nested item",
              "    ```html-artifact",
              "    <div>nested list preview</div>",
              "    ```",
              "",
              "- ```html-artifact",
              "  <div>inline list preview</div>",
              "  ```",
            ].join("\n"),
          },
        ],
        [],
      ),
    }),
  ]);

  expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
    "/v1/artifacts/24/0/1",
    "/v1/artifacts/24/0/2",
    "/v1/artifacts/24/0/3",
    "/v1/artifacts/24/0/4",
  ]);
});

test("user, tool-result, and live text fences never resolve as artifacts", async () => {
  const fetchMock = stubArtifactResolution();
  const fence = (label: string) =>
    `\`\`\`html-artifact\n<div>${label}</div>\n\`\`\``;
  const html = await markupAfterEffects(
    <Transcript
      nodes={buildTree([
        entry(30, { User: [{ Text: fence("user prose") }] }),
        entry(31, {
          Assistant: messagesAssistant(
            [],
            [],
            [{ id: "call", name: "echo", input: {} }],
          ),
        }),
        entry(32, {
          ToolResult: {
            call_id: "call",
            raw: { content: fence("tool output"), is_error: false },
            wire: "Raw",
          },
        }),
      ])}
      streaming={{
        ...emptyStreaming,
        blocks: [{ index: 0, kind: "text", text: fence("live text") }],
      }}
      pending={[]}
      ask={null}
      gatedCall={null}
      artifactEnabled
      onGateSlot={() => {}}
      onRecall={() => {}}
      onInspect={() => {}}
      onOpenFile={() => {}}
    />,
  );

  expect(fetchMock).not.toHaveBeenCalled();
  expect(html).not.toContain("<iframe");
  expect(html).toContain("user prose");
  expect(html).toContain("tool output");
  expect(html).toContain("live text");
});

test("a resolver failure leaves source and copy access available", async () => {
  const fetchMock = stubArtifactResolution(404);
  const html = await renderWithArtifacts([
    entry(33, {
      Assistant: messagesAssistant(
        [],
        [{ Text: "```html-artifact\n<div>retained source</div>\n```" }],
        [],
      ),
    }),
  ]);

  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(html).toContain(
    "Preview unavailable; source is still available below.",
  );
  expect(html).toContain("Source");
  expect(html).toContain("Copy source");
  expect(html).toContain("retained source");
  expect(html).not.toContain("<iframe");
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

test("a turn another agent sent is labelled with its sender", () => {
  const sourced = (seq: number, source?: string): LogEntry => ({
    ...entry(seq, { User: [{ Text: `turn ${seq}` }] }),
    ...(source === undefined ? {} : { source }),
  });
  const html = render([
    sourced(1),
    sourced(2, "web"),
    sourced(3, "mcp[claude-code]"),
    sourced(4, "session[parent]"),
  ]);
  expect(html.match(/>you</g)?.length).toBe(2);
  expect(html).toContain('class="entry user agent"');
  expect(html).toContain('title="mcp[claude-code]">claude-code<');
  expect(html).toContain('title="session[parent]">parent<');
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

test("shows the exact working span and counts only entries in that range", () => {
  const html = render([
    entry(1, { User: [{ Text: "retained prefix" }] }),
    entry(2, { User: [{ Text: "work" }] }),
    entry(3, { User: [{ Text: "more work" }] }),
    entry(4, {
      Summary: { from: 2, upto: 3, text: "account" },
    }),
  ]);
  expect(html).toContain("consolidated #2–#3");
  expect(html).toContain("2 entries");
  expect(html).toContain("account");
});

test.each([
  [{ source: "web", control: "stop-button" }, "the web stop button"],
  [
    { source: "web", control: "status-bar-stop" },
    "the web status-bar stop button",
  ],
  [{ control: "esc" }, "TUI Esc"],
  [{ control: "control-cli" }, "the command line"],
  [{ source: "session[calm-finch]" }, "session calm-finch"],
  [{ source: "mcp[Claude Code]" }, "MCP client Claude Code"],
  [{}, "the command line or TUI"],
  [{ source: "web" }, "web"],
  [{ source: "other", control: "custom" }, "other (custom)"],
  [{ control: "custom" }, "the command line or TUI (custom)"],
])("names an interrupt origin %j", (origin, label) => {
  const html = render([
    entry(1, { Telemetry: { Interrupt: origin } }),
    entry(2, {
      Assistant: {
        ...messagesAssistant([], [], []),
        excluded: { Upstream: "generation interrupted" },
      },
    }),
  ]);
  expect(html).toContain(
    `Generation interrupted from ${label} — recorded, not in context`,
  );
});

test("does not attribute an interrupt without an origin record", () => {
  const html = render([
    entry(1, {
      Assistant: {
        ...messagesAssistant([], [], []),
        excluded: { Upstream: "generation interrupted" },
      },
    }),
  ]);
  expect(html).toContain("Generation interrupted — recorded, not in context");
  expect(html).not.toContain("interrupted from");
});
