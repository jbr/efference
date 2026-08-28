import { describe, expect, it } from "vitest";
import { matchPath, matchPaths } from "./pathMatch.ts";

const paths = [
  "backlog/INDEX.md",
  "docs/web-client.md",
  "web/src/ui/FileView.tsx",
  "web/src/session/lineDiff.ts",
  "node_modules/react/index.js",
];

describe("matchPath", () => {
  it("matches a subsequence, not just a substring", () => {
    expect(matchPath("wsfv", "web/src/ui/FileView.tsx")).not.toBeNull();
    expect(matchPath("zzz", "web/src/ui/FileView.tsx")).toBeNull();
  });

  it("is case-insensitive until the query has a capital in it", () => {
    expect(matchPath("index", "backlog/INDEX.md")).not.toBeNull();
    expect(matchPath("INDEX", "backlog/index.md")).toBeNull();
  });

  it("matches everything on an empty query", () => {
    expect(matchPath("", "anything")).toEqual({
      path: "anything",
      score: 0,
      matched: [],
    });
  });
});

describe("matchPaths", () => {
  it("ranks the basename hit above an incidental one", () => {
    expect(matchPaths("fileview", paths)[0].path).toBe(
      "web/src/ui/FileView.tsx",
    );
  });

  it("finds a whole path faster than three expansions", () => {
    expect(matchPaths("backlog/INDEX.md", paths)[0].path).toBe(
      "backlog/INDEX.md",
    );
  });

  it("honours the limit", () => {
    expect(matchPaths("", paths, 2)).toHaveLength(2);
  });
});
