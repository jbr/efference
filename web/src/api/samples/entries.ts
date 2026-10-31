// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// Log entries are the three things the entry view adds to a stored entry (src/api/entry.ts),
// in the shapes src/daemon/entry_view/test.rs builds and this project's own sessions write.

import type { LogEntry } from "../entry.ts";

export const entries: {
  logEntries: LogEntry[];
} = {
  logEntries: [
    // A verdict that keeps every line of the result it decides.
    {
      seq: 115,
      turn: 8,
      at: 1792186748514,
      payload: {
        ResultVerdict: {
          for_result: 110,
          selection: { kept_lines: 1, total_lines: 1 },
          wire: {
            Triaged: {
              intent_feedback: "the listing is what was asked for",
              keep: "All",
              note: "one line",
            },
          },
        },
      },
    },
    // A result cut at 16 KiB: a prefix of `content`, the tag that says so, and the line
    // arithmetic against the whole.
    {
      seq: 39,
      turn: 11,
      at: 1789271877600,
      payload: {
        ToolResult: {
          call_id: "toolu_1",
          raw: {
            content: "line 1\nline 2\n",
            content_elided: { kept_bytes: 16384, total_bytes: 25786 },
            is_error: false,
          },
          selection: { kept_lines: 645, total_lines: 645 },
          wire: {
            Triaged: {
              intent_feedback: null,
              keep: "All",
              note: "the whole file",
            },
          },
        },
      },
    },
    // A result with an image: its dimensions, and no bytes.
    {
      seq: 12,
      turn: 3,
      at: 1789271877700,
      payload: {
        ToolResult: {
          call_id: "toolu_2",
          raw: {
            content: "screenshot.png",
            is_error: false,
            images: [{ media_type: "image/webp", width: 40, height: 30 }],
          },
          wire: "Raw",
        },
      },
    },
    // A committed assistant entry whose text has an artifact fence.
    {
      seq: 20,
      turn: 4,
      at: 1789271877800,
      payload: {
        Assistant: {
          output: { Messages: [{ Text: "```mermaid\ngraph TD\n```" }] },
          artifacts: [
            {
              text_block: 0,
              fence: 0,
              kind: "mermaid",
              url: "/artifacts/amber-heron/20/0/0/0123456789abcdef",
              external_url:
                "http://127.0.0.1:7979/amber-heron/20/0/0/0123456789abcdef",
            },
          ],
        },
      },
    },
    // A turn with an attached file.
    {
      seq: 381,
      turn: 41,
      at: 1792977091962,
      source: "web",
      submission_id: "sub-1",
      payload: {
        User: [
          { Text: "what is in this?" },
          {
            Attachment: {
              kind: {
                Image: {
                  bytes: 2730985,
                  format: "png",
                  height: 2334,
                  width: 2816,
                },
              },
              name: "shot.png",
              path: "/work/.efference/amber-heron/attachments/shot.png",
            },
          },
        ],
      },
    },
  ],
};
