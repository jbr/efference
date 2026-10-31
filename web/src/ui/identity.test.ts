import { handoffPredecessor } from "../api/source.ts";
import { describe, expect, it } from "vitest";

import { successorHref, homeRelative } from "./identity.ts";

describe("homeRelative", () => {
  it("shortens a path under a recognized home layout", () => {
    expect(homeRelative("/Users/jbr/code/efference")).toBe("~/code/efference");
    expect(homeRelative("/home/jbr/code/efference")).toBe("~/code/efference");
    expect(homeRelative("/Users/jbr")).toBe("~");
  });

  it("leaves anything it does not recognize verbatim", () => {
    // A wrongly shortened path names a directory that does not exist, so the
    // unrecognized cases stay absolute rather than guessing.
    expect(homeRelative("/srv/build")).toBe("/srv/build");
    expect(homeRelative("/Users")).toBe("/Users");
    expect(homeRelative("/homework/x")).toBe("/homework/x");
    expect(homeRelative("relative/path")).toBe("relative/path");
  });
});

describe("successorHref", () => {
  // A session that handed off (docs/tools.md) sends its viewer to the successor.
  it("names the successor's page once there is one", () => {
    const href = (id: string) => `/sessions/${id}/`;
    expect(successorHref({ successor: "new-leaf" }, href)).toBe(
      "/sessions/new-leaf/",
    );
    expect(successorHref({}, href)).toBe(null);
    expect(successorHref(null, href)).toBe(null);
  });
});

describe("handoffPredecessor", () => {
  it("reads the reserved hand-off source and nothing else", () => {
    expect(handoffPredecessor("handoff[old-leaf]")).toBe("old-leaf");
    expect(handoffPredecessor("web")).toBe(null);
    expect(handoffPredecessor("agent[old-leaf]")).toBe(null);
    expect(handoffPredecessor(undefined)).toBe(null);
  });
});
