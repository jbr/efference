// A transcript whose values are longer than the phone is wide: a path chip
// with a long path, a bash call, and a result line with no break opportunity.
// Resize the page to 390px (or open it on a phone) and the document should
// stay as wide as the screen; `?overflow` lists any element whose content
// spills (docs/web-client.md "A transcript item never widens the page").

import type { LogEntry } from "../api/types.ts";
import { messagesAssistant } from "../session/assistant.ts";
import { emptyStreaming } from "../session/store.ts";
import { buildTree } from "../session/tree.ts";
import { summaryPreviews } from "../session/summaryPreviews.ts";
import { TranscriptContent } from "../ui/Transcript.tsx";

const noop = () => {};
const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 1_760_000_000_000,
  payload,
});

const write = {
  id: "gallery-overflow-write",
  name: "write",
  input: {
    intent: "Record the follow-tail decision",
    path: "journal/2026-10-10-web-follow-tail-intent-and-the-long-unbroken-name.md",
    contents: "# Follow tail\n\nThe reader's intent decides.",
    append: true,
  },
};
const readLong = {
  id: "gallery-overflow-read",
  name: "read",
  input: {
    intent: "Read the compaction note",
    paths: ["backlog/inbox/manual-compaction-worker-without-cache-metadata.md"],
  },
};
const shell = {
  id: "gallery-overflow-bash",
  name: "bash",
  input: {
    intent: "Print the digest",
    command: "sha512sum target/release/efference-with-a-very-long-name",
  },
};
const unbroken =
  "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a089f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

const entries: LogEntry[] = [
  entry(1, { User: [{ Text: "Check the long values on a phone." }] }),
  entry(2, {
    Assistant: messagesAssistant([], [], [write, readLong, shell]),
  }),
  entry(3, {
    ToolResult: {
      call_id: write.id,
      raw: { content: `Wrote ${write.input.path}`, is_error: false },
      wire: "Raw",
    },
  }),
  entry(4, {
    ToolResult: {
      call_id: readLong.id,
      raw: { content: "1\t# Manual compaction", is_error: false },
      wire: "Raw",
    },
  }),
  entry(5, {
    ToolResult: {
      call_id: shell.id,
      raw: {
        content: `${unbroken}  target/release/efference-with-a-very-long-name`,
        is_error: false,
      },
      wire: "Raw",
    },
  }),
];

export function OverflowSpecimens() {
  return (
    <>
      <p className="g-intro">
        Values with no break opportunity or longer than a phone: the page keeps
        the screen’s width and an overrunning element is clipped at its item’s
        edge. Resize to 390px to see it.
      </p>
      <TranscriptContent
        nodes={buildTree(
          entries,
          summaryPreviews(entries, emptyStreaming, null),
        )}
        streaming={emptyStreaming}
        pending={[]}
        ask={null}
        gatedCall={null}
        onGateSlot={noop}
        onRecall={noop}
        onInspect={noop}
        onOpenFile={noop}
      />
    </>
  );
}
