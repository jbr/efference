import { describe, expect, it } from "vitest";
import { splitFrontmatter } from "./frontmatter.ts";

describe("splitFrontmatter", () => {
  it("lifts the fields and blanks their lines", () => {
    const text = '---\nsummary: "a \\"b\\""\ntouches: src/x.rs\n---\n# Title\n';
    const { fields, body } = splitFrontmatter(text);
    expect(fields).toEqual([
      ["summary", 'a "b"'],
      ["touches", "src/x.rs"],
    ]);
    expect(body).toBe("\n\n\n\n# Title\n");
    expect(body.split("\n").length).toBe(text.split("\n").length);
  });

  it("leaves a file without a leading block alone", () => {
    expect(splitFrontmatter("# Title\n---\n")).toEqual({
      fields: null,
      body: "# Title\n---\n",
    });
    expect(splitFrontmatter("---\nno close\n")).toEqual({
      fields: null,
      body: "---\nno close\n",
    });
  });

  it("keeps a line that is not key: value", () => {
    expect(splitFrontmatter("---\n- item\n---\n").fields).toEqual([
      ["", "- item"],
    ]);
  });
});
