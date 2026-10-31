// The grouped view of an assistant entry: thinking, prose, and calls, each in
// provider order. It mirrors the accessors on `AssistantOutput` in src/log.rs
// (`reasoning_text`, `text`, `tool_calls`) so the transcript renders one shape
// whichever wire dialect answered. Provider replay credentials — a Messages
// thinking signature, a Responses `encrypted_content` — are not part of the
// view. Ordered rendering across the three groups is deferred with the TUI's
// (backlog/inbox/openai-provider/ordered-clients.md).

import type {
  AssistantOutput,
  CompletionsMessage,
  ContentBlock,
  MessagesOutputBlock,
  ToolCall,
} from "../api/types.ts";
import type { Payload } from "../api/entry.ts";

/** A block of thinking or prose: a `ContentBlock` without the composer's
 * attachment, which only a user turn carries. */
export type ProseBlock = Exclude<ContentBlock, { Attachment: unknown }>;

export interface AssistantView {
  thinking: ProseBlock[];
  text: ProseBlock[];
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
  if ("Completions" in output) return completionsView(output.Completions);
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

/** The keys the Completions dialect defines; every other key is opaque vendor
 * reasoning (src/log/completions.rs, `DEFINED`). */
const DEFINED = new Set(["role", "content", "tool_calls", "refusal"]);

/** Keys whose string values are read out of an opaque structure (`READABLE`). */
const READABLE = new Set(["text", "summary", "content", "thinking"]);

/** The keys the dialect defines, as the transcript reads a stored message. The
 * message is a map of whatever the vendor sent (`CompletionsMessage` is open),
 * so this is a reading of it, not a promise the schema makes. */
interface CompletionsKeys {
  content?: string | { type: string; [extra: string]: unknown }[];
  tool_calls?: {
    id?: string;
    function?: { name?: string; arguments?: string };
    [extra: string]: unknown;
  }[];
  refusal?: string;
  [extra: string]: unknown;
}

/** Mirrors `CompletionsMessage::{text, reasoning_text, tool_calls}`. Reasoning is
 * a display heuristic: a flat string under an opaque key wins; only when there is
 * none are structured keys and non-`text` content parts walked, since OpenRouter
 * sends the same reasoning both flat and as `reasoning_details`. */
function completionsView(stored: CompletionsMessage): AssistantView {
  const message = stored as CompletionsKeys;
  const view: AssistantView = { thinking: [], text: [], calls: [] };
  const parts = Array.isArray(message.content) ? message.content : [];
  if (typeof message.content === "string") {
    view.text.push({ Text: message.content });
  }
  for (const part of parts) {
    if (part.type === "text" && typeof part.text === "string") {
      view.text.push({ Text: part.text });
    }
  }
  if (typeof message.refusal === "string") {
    view.text.push({ Text: message.refusal });
  }
  view.text = view.text.filter((block) => "Text" in block && block.Text !== "");

  const opaque = Object.entries(message).filter(([key]) => !DEFINED.has(key));
  let reasoning = opaque
    .map(([, value]) => value)
    .filter((value): value is string => typeof value === "string");
  if (reasoning.every((text) => text === "")) {
    reasoning = [];
    for (const [, value] of opaque) readable(value, reasoning);
    for (const part of parts)
      if (part.type !== "text") readable(part, reasoning);
  }
  view.thinking = reasoning
    .filter((text) => text !== "")
    .map((text) => ({ Text: text }));

  for (const call of message.tool_calls ?? []) {
    const args = call.function?.arguments ?? "";
    view.calls.push({
      id: call.id ?? "",
      name: call.function?.name ?? "",
      input: args.trim() === "" ? {} : parseArguments(args),
    });
  }
  return view;
}

/** The strings under READABLE keys inside `value`, depth first in key order. */
function readable(value: unknown, out: string[]): void {
  if (Array.isArray(value)) {
    for (const item of value) readable(item, out);
  } else if (value !== null && typeof value === "object") {
    for (const [key, inner] of Object.entries(value)) {
      if (typeof inner === "string" && READABLE.has(key)) out.push(inner);
      else readable(inner, out);
    }
  }
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
  thinking: ProseBlock[],
  text: ProseBlock[],
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
