import { describe, expect, it } from "vitest";

import { homeRelative } from "./identity.ts";

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
