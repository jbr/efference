import { describe, expect, it } from "vitest";
import { resolveLink } from "./fileLinks.ts";

describe("resolveLink", () => {
  it("resolves against the linking file's directory", () => {
    expect(resolveLink("backlog/INDEX.md", "README.md")).toEqual({
      path: "backlog/README.md",
      dir: false,
    });
    expect(resolveLink("CLAUDE.md", "docs/README.md")).toEqual({
      path: "docs/README.md",
      dir: false,
    });
  });

  it("climbs with .. but not past the root", () => {
    expect(resolveLink("backlog/inbox/a.md", "../../docs/x.md")).toEqual({
      path: "docs/x.md",
      dir: false,
    });
    expect(resolveLink("backlog/INDEX.md", "../../etc/passwd")).toBeNull();
  });

  it("keeps a trailing slash as a directory", () => {
    expect(resolveLink("backlog/INDEX.md", "inbox/")).toEqual({
      path: "backlog/inbox",
      dir: true,
    });
  });

  it("drops a fragment and a query, and decodes", () => {
    expect(resolveLink("docs/a.md", "b%20c.md#section")).toEqual({
      path: "docs/b c.md",
      dir: false,
    });
  });

  it("leaves URLs, absolute paths, and bare fragments to the browser", () => {
    expect(resolveLink("a.md", "https://example.com/x.md")).toBeNull();
    expect(resolveLink("a.md", "mailto:hi@example.com")).toBeNull();
    expect(resolveLink("a.md", "//example.com/x")).toBeNull();
    expect(resolveLink("a.md", "/abs.md")).toBeNull();
    expect(resolveLink("a.md", "#heading")).toBeNull();
  });
});
