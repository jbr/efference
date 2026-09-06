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
