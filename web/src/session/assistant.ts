// The grouped view of an assistant entry: thinking, prose, and calls, each in
// provider order. It mirrors the accessors on `AssistantOutput` in src/log.rs
// (`reasoning_text`, `text`, `tool_calls`) so the transcript renders one shape
// whichever wire dialect answered. Provider replay credentials — a Messages
// thinking signature, a Responses `encrypted_content` — are not part of the
// view. Ordered rendering across the three groups is deferred with the TUI's
// (backlog/openai-provider/ordered-clients.md).

import type {
  AssistantOutput,
  ContentBlock,
  MessagesOutputBlock,
  Payload,
  ToolCall,
} from "../api/types.ts";

export interface AssistantView {
  thinking: ContentBlock[];
  text: ContentBlock[];
  calls: ToolCall[];
}

type AssistantPayload = Extract<Payload, { Assistant: unknown }>["Assistant"];

/** Group one entry's output. Responses argument strings are parsed here; one
 * the provider emitted malformed renders as the raw string under `arguments`
 * rather than dropping the call, since its result still needs a home. */
export function assistantView(assistant: AssistantPayload): AssistantView {
  return viewOf(assistant.output);
}

export function viewOf(output: AssistantOutput): AssistantView {
  const view: AssistantView = { thinking: [], text: [], calls: [] };
  if ("Messages" in output) {
    for (const block of output.Messages) {
      if ("Text" in block) view.text.push(block);
      else if ("Thinking" in block) view.thinking.push(block);
      else if ("RedactedThinking" in block) view.thinking.push(block);
      else if ("ToolUse" in block) view.calls.push(block.ToolUse);
    }
    return view;
  }
  for (const item of output.Responses) {
    if ("Reasoning" in item) {
      for (const part of item.Reasoning.summary) {
        view.thinking.push({ Text: part.text });
      }
    } else if ("Message" in item) {
      for (const content of item.Message.content) {
        if ("OutputText" in content) {
          view.text.push({ Text: content.OutputText.text });
        } else if ("Refusal" in content) {
          view.text.push({ Text: content.Refusal.refusal });
        }
      }
    } else if ("FunctionCall" in item) {
      const { call_id, name, arguments: args } = item.FunctionCall;
      view.calls.push({ id: call_id, name, input: parseArguments(args) });
    }
  }
  return view;
}

function parseArguments(args: string): unknown {
  try {
    return JSON.parse(args) as unknown;
  } catch {
    return { arguments: args };
  }
}

/** A Messages-dialect payload from grouped blocks — the fixture convenience
 * test suites build entries with (the counterpart of `from_legacy_messages`).
 * A bare `Text` given as thinking becomes an unsigned `Thinking` block, since
 * the dialect has no other place for it. */
export function messagesAssistant(
  thinking: ContentBlock[],
  text: ContentBlock[],
  calls: ToolCall[],
): { output: AssistantOutput } {
  return {
    output: {
      Messages: [
        ...thinking.map((block): MessagesOutputBlock =>
          "Text" in block
            ? { Thinking: { text: block.Text, signature: "" } }
            : block,
        ),
        ...text,
        ...calls.map((call) => ({ ToolUse: call })),
      ],
    },
  };
}
