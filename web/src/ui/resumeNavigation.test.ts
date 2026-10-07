import { describe, expect, it } from "vitest";
import { resumePage, resumeUrl } from "./resumeNavigation.ts";

describe("resume routes", () => {
  it("parses resume page from search string", () => {
    expect(resumePage("")).toBeNull();
    expect(resumePage("?new=location")).toBeNull();
    expect(resumePage("?resume=location")).toEqual({ stage: "location" });
    expect(resumePage("?resume=picker&cwd=%2FUsers%2Fjbr%2Fcode")).toEqual({
      stage: "picker",
      cwd: "/Users/jbr/code",
    });
    // picker without cwd defaults to null
    expect(resumePage("?resume=picker")).toBeNull();
  });

  it("builds resume URLs correctly", () => {
    const current = "https://example.com/?view=all#top";
    expect(resumeUrl(current, null)).toBe("/?view=all#top");
    expect(resumeUrl(current, { stage: "location" })).toBe(
      "/?view=all&resume=location#top",
    );
    expect(
      resumeUrl(current, { stage: "picker", cwd: "/Users/jbr/code/efference" }),
    ).toBe(
      "/?view=all&resume=picker&cwd=%2FUsers%2Fjbr%2Fcode%2Fefference#top",
    );
  });
});
