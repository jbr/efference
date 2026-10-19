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
  it("matches each word as written, not scattered letters", () => {
    expect(matchPath("fileview", "web/src/ui/FileView.tsx")).not.toBeNull();
    expect(matchPath("wsfv", "web/src/ui/FileView.tsx")).toBeNull();
    expect(matchPath("rough", "assets/svg/gfx_btn-group-right.svg")).toBeNull();
  });

  it("needs every word, in any order", () => {
    expect(matchPath("view ui", "web/src/ui/FileView.tsx")?.matched).toEqual([
      [8, 10],
      [15, 19],
    ]);
    expect(matchPath("view docs", "web/src/ui/FileView.tsx")).toBeNull();
  });

  it("merges overlapping words into one span", () => {
    expect(matchPath("file view fileview", "FileView.tsx")?.matched).toEqual([
      [0, 8],
    ]);
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
