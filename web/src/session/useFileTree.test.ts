import { describe, expect, it } from "vitest";
import { ancestors } from "./useFileTree.ts";

describe("ancestors", () => {
  it("names every directory to open to reveal a path, root first", () => {
    expect(ancestors("web/src/ui/FileView.tsx")).toEqual([
      "",
      "web",
      "web/src",
      "web/src/ui",
    ]);
  });

  it("is the root alone for a file at the top", () => {
    expect(ancestors("README.md")).toEqual([""]);
  });
});
