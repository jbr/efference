import { describe, expect, it } from "vitest";
import { messagesAssistant, viewOf } from "./assistant.ts";

describe("the grouped assistant view", () => {
  it("groups Messages blocks by kind, each in provider order", () => {
    const view = viewOf({
      Messages: [
        { Thinking: { text: "hm", signature: "sig" } },
        { Text: "one" },
        { ToolUse: { id: "c1", name: "bash", input: { command: "ls" } } },
        { Text: "two" },
        { RedactedThinking: { data: "opaque" } },
      ],
    });
    expect(view.thinking).toEqual([
      { Thinking: { text: "hm", signature: "sig" } },
      { RedactedThinking: { data: "opaque" } },
    ]);
    expect(view.text).toEqual([{ Text: "one" }, { Text: "two" }]);
    expect(view.calls).toEqual([
      { id: "c1", name: "bash", input: { command: "ls" } },
    ]);
  });

  it("reads Responses items as the same view, parsing argument strings", () => {
    const view = viewOf({
      Responses: [
        {
          Reasoning: {
            id: "rs_1",
            summary: [{ text: "**Planning**" }],
            encrypted_content: "gAAAA",
          },
        },
        {
          Message: {
            id: "msg_1",
            status: "completed",
            content: [
              { OutputText: { text: "hello", annotations: [] } },
              { Refusal: { refusal: "no" } },
            ],
          },
        },
        {
          FunctionCall: {
            id: "fc_1",
            call_id: "call_1",
            name: "bash",
            arguments: '{"command":"ls"}',
            status: "completed",
          },
        },
        { Unsupported: { type: "web_search_call" } },
      ],
    });
    expect(view.thinking).toEqual([{ Text: "**Planning**" }]);
    expect(view.text).toEqual([{ Text: "hello" }, { Text: "no" }]);
    expect(view.calls).toEqual([
      { id: "call_1", name: "bash", input: { command: "ls" } },
    ]);
  });

  it("keeps a call whose argument string is not JSON", () => {
    const view = viewOf({
      Responses: [
        {
          FunctionCall: { call_id: "call_1", name: "bash", arguments: "{oops" },
        },
      ],
    });
    expect(view.calls).toEqual([
      { id: "call_1", name: "bash", input: { arguments: "{oops" } },
    ]);
  });

  it("reads a Completions message: content, flat reasoning, calls", () => {
    const view = viewOf({
      Completions: {
        content: "reading",
        reasoning_content: "let me look",
        tool_calls: [
          {
            id: "call_00",
            type: "function",
            function: { name: "read", arguments: '{"paths": ["a"]}' },
          },
          {
            id: "call_01",
            type: "function",
            function: { name: "list", arguments: "" },
          },
        ],
      },
    });
    expect(view.text).toEqual([{ Text: "reading" }]);
    expect(view.thinking).toEqual([{ Text: "let me look" }]);
    expect(view.calls).toEqual([
      { id: "call_00", name: "read", input: { paths: ["a"] } },
      { id: "call_01", name: "list", input: {} },
    ]);
  });

  it("prefers flat Completions reasoning over its structured duplicate", () => {
    const view = viewOf({
      Completions: {
        content: "",
        reasoning: "thought",
        reasoning_details: [{ type: "reasoning.text", text: "thought" }],
      },
    });
    expect(view.text).toEqual([]);
    expect(view.thinking).toEqual([{ Text: "thought" }]);
  });

  it("reads Completions thinking content parts beside text parts", () => {
    const view = viewOf({
      Completions: {
        content: [
          { type: "thinking", thinking: [{ type: "text", text: "hm" }] },
          { type: "text", text: "answer" },
        ],
      },
    });
    expect(view.text).toEqual([{ Text: "answer" }]);
    expect(view.thinking).toEqual([{ Text: "hm" }]);
  });

  it("round-trips the grouped fixture through the Messages dialect", () => {
    const call = { id: "c1", name: "write", input: {} };
    const { output } = messagesAssistant(
      [{ Text: "hm" }],
      [{ Text: "p" }],
      [call],
    );
    expect(viewOf(output)).toEqual({
      thinking: [{ Thinking: { text: "hm", signature: "" } }],
      text: [{ Text: "p" }],
      calls: [call],
    });
  });
});
