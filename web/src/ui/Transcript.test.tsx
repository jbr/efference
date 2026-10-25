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
import type { Streaming } from "../session/store.ts";
import { buildTree } from "../session/tree.ts";
import { Transcript } from "./Transcript.tsx";
import fenceCases from "./artifact-fences.json";

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

/** The session transcript after its effects, with each rendered artifact's
 * frame URL and the source shown beside it, in order. */
async function renderWithArtifacts(
  entries: LogEntry[],
  over: Parameters<typeof element>[1] = {},
): Promise<{ html: string; artifacts: { url: string; source: string }[] }> {
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () => {
    root.render(element(entries, { ...over, artifactEnabled: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  const artifacts = [...host.querySelectorAll(".artifact")].map((artifact) => ({
    url: artifact.querySelector("iframe")?.getAttribute("src") ?? "",
    source: artifact.querySelector(".artifact-source code")?.textContent ?? "",
  }));
  const html = host.innerHTML;
  act(() => root.unmount());
  return { html, artifacts };
}

/** A committed assistant entry as the entry view serves it: text blocks, and a
 * link for each `[textBlock, fence]` given (docs/artifacts.md). */
const withArtifacts = (
  seq: number,
  blocks: string[],
  linked: [number, number][],
): LogEntry =>
  entry(seq, {
    Assistant: {
      ...messagesAssistant(
        [],
        blocks.map((Text) => ({ Text })),
        [],
      ),
      artifacts: linked.map(([text_block, fence]) => ({
        text_block,
        fence,
        url: `/artifacts/brisk-otter/${seq}/${text_block}/${fence}/tag`,
      })),
    },
  });

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

test("a fence the entry view links renders as a sandboxed frame with its source", async () => {
  const { html, artifacts } = await renderWithArtifacts([
    withArtifacts(
      20,
      ["```html-artifact\n<div>preview content</div>\n```\n"],
      [[0, 0]],
    ),
  ]);

  expect(artifacts).toEqual([
    {
      url: "/artifacts/brisk-otter/20/0/0/tag",
      source: "<div>preview content</div>",
    },
  ]);
  expect(html).toContain('sandbox="allow-scripts"');
  expect(html).not.toContain("allow-same-origin");
  expect(html).toContain("Copy source");
});

// SessionView rebuilds the transcript tree on every streamed token. A committed
// entry's frame that remounted then would reload on each one.
test("a streaming delta leaves committed artifact frames and code blocks mounted", async () => {
  const entries = [
    withArtifacts(
      25,
      [
        "```html-artifact\n<div>chart</div>\n```\n\n```rust\nfn main() {}\n```\n",
      ],
      [[0, 0]],
    ),
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
  act(() => root.unmount());
});

test.each([
  ["ordinary HTML", "```html\n<div>ordinary HTML</div>\n```", "ordinary HTML"],
  ["ordinary SVG", "```svg\n<svg>ordinary SVG</svg>\n```", "ordinary SVG"],
  ["an unlabelled fence", "```\nplain code\n```", "plain code"],
  [
    "an indented fence-like block",
    "    ```html-artifact\n    not a fence\n    ```",
    "not a fence",
  ],
  [
    "html-artifact with more info",
    "```html-artifact preview\n<div>source only</div>\n```",
    "source only",
  ],
])(
  "%s stays source beside a linked artifact",
  async (_label, source, expected) => {
    const { html, artifacts } = await renderWithArtifacts([
      withArtifacts(
        21,
        [`${source}\n\n\`\`\`html-artifact\n<div>linked</div>\n\`\`\`\n`],
        [[0, 0]],
      ),
    ]);

    expect(artifacts).toEqual([
      { url: "/artifacts/brisk-otter/21/0/0/tag", source: "<div>linked</div>" },
    ]);
    expect(html).toContain(expected);
  },
);

test("fences are numbered per text block, counting artifact fences only", async () => {
  const { artifacts } = await renderWithArtifacts([
    withArtifacts(
      23,
      [
        "```javascript\nconst before = true;\n```\n\n```html-artifact\n<div>first</div>\n```\n",
        "```html-artifact\n<div>second</div>\n```\n",
      ],
      [
        [0, 0],
        [1, 0],
      ],
    ),
  ]);

  expect(artifacts).toEqual([
    { url: "/artifacts/brisk-otter/23/0/0/tag", source: "<div>first</div>" },
    { url: "/artifacts/brisk-otter/23/1/0/tag", source: "<div>second</div>" },
  ]);
});

// The server's extractor is tested on the same cases (src/artifact/test.rs), so
// the two CommonMark parsers agree on which fences a link names.
test.each(fenceCases.map((c) => [c.case, c] as const))(
  "fence numbering matches the server's: %s",
  async (_label, { markdown, fences }) => {
    const { artifacts } = await renderWithArtifacts([
      withArtifacts(
        40,
        [markdown],
        fences.map((_, fence): [number, number] => [0, fence]),
      ),
    ]);

    expect(artifacts).toEqual(
      fences.map((source, fence) => ({
        url: `/artifacts/brisk-otter/40/0/${fence}/tag`,
        source,
      })),
    );
  },
);

test("an artifact fence with no link stays code", async () => {
  const { html, artifacts } = await renderWithArtifacts([
    withArtifacts(
      33,
      [
        "```html-artifact\n<div>linked</div>\n```\n\n```html-artifact\n<div>unlinked</div>\n```\n",
      ],
      [[0, 0]],
    ),
  ]);

  expect(artifacts).toEqual([
    { url: "/artifacts/brisk-otter/33/0/0/tag", source: "<div>linked</div>" },
  ]);
  expect(html).toContain('data-lang="html"');
  expect(html).toContain("unlinked");
});

test("user, tool-result, and live text fences never render as artifacts", async () => {
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

  expect(html).not.toContain("<iframe");
  expect(html).toContain("user prose");
  expect(html).toContain("tool output");
  expect(html).toContain("live text");
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
