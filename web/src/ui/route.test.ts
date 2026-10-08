import { describe, expect, it } from "vitest";
import { isClientPath } from "./route.ts";

describe("which paths the client navigates itself", () => {
  it("takes the list and a session's own view", () => {
    expect(isClientPath("/")).toBe(true);
    expect(isClientPath("/sessions/a-b/")).toBe(true);
    expect(isClientPath("/sessions/a-b")).toBe(true);
  });

  it("leaves routes inside a session and the listener's own paths to the browser", () => {
    expect(isClientPath("/sessions/a-b/tree")).toBe(false);
    expect(isClientPath("/sessions/a-b/v1/state")).toBe(false);
    expect(isClientPath("/auth/session")).toBe(false);
    expect(isClientPath("/sessions/")).toBe(false);
    expect(isClientPath("/sessions/../x/")).toBe(false);
  });
});
