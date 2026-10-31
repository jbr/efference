import { describe, expect, test } from "vitest";
import { senderName, sessionSource } from "./source.ts";

describe("a session source", () => {
  test("names another project only when it was sent from one", () => {
    expect(sessionSource("session[calm-finch]")).toEqual({
      id: "calm-finch",
      place: null,
    });
    expect(sessionSource("session[calm-finch in ~/code/other]")).toEqual({
      id: "calm-finch",
      place: "~/code/other",
    });
    expect(sessionSource("mcp[claude-code]")).toBeNull();
  });

  test("is named by its id in the transcript", () => {
    expect(senderName("session[calm-finch in ~/code/other]")).toBe(
      "calm-finch",
    );
    expect(senderName("session[calm-finch]")).toBe("calm-finch");
    expect(senderName("mcp[Claude Desktop]")).toBe("Claude Desktop");
    expect(senderName("web")).toBe("you");
  });
});
