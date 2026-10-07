import { describe, expect, it } from "vitest";
import { formatAge, partitionArchived, filterArchived } from "./resume.ts";
import type { ArchivedSession } from "../api/http.ts";

describe("formatAge", () => {
  it("formats relative age correctly", () => {
    const now = 10000000;
    expect(formatAge(now, now - 30 * 1000)).toBe("just now");
    expect(formatAge(now, now - 5 * 60 * 1000)).toBe("5m ago");
    expect(formatAge(now, now - 2 * 3600 * 1000)).toBe("2h ago");
    expect(formatAge(now, now - 3 * 86400 * 1000)).toBe("3d ago");
  });
});

describe("partitionArchived", () => {
  const session = (
    id: string,
    ageMs: number,
    problem?: string,
  ): ArchivedSession => ({
    id,
    cwd: "/Users/test/project",
    title: `Session ${id}`,
    last_active_at: 10000000 - ageMs,
    state: "archived",
    problem,
  });

  it("partitions into recent cards, listed rows, and remainder", () => {
    const now = 10000000;
    const items: ArchivedSession[] = [
      session("recent-1", 10 * 60 * 1000), // 10m ago (Band 2)
      session("recent-2", 40 * 60 * 1000), // 40m ago (Band 2)
      session("listed-1", 2 * 3600 * 1000), // 2h ago (Band 3)
      session("listed-2", 5 * 3600 * 1000), // 5h ago (Band 3)
      session("listed-3", 24 * 3600 * 1000), // 1d ago (Band 3)
      session("listed-4", 2 * 86400 * 1000), // 2d ago (Band 3)
      session("listed-5", 3 * 86400 * 1000), // 3d ago (Band 3)
      session("extra-1", 4 * 86400 * 1000), // 4d ago (Band 4)
      session("broken", 5 * 60 * 1000, "broken log"), // has problem -> not offered
    ];

    const { recentCards, listedRows, totalCount, remainderCount } =
      partitionArchived(
        items,
        now,
        false, // not narrow
      );

    expect(recentCards.map((s) => s.id)).toEqual(["recent-1", "recent-2"]);
    expect(listedRows.map((s) => s.id)).toEqual([
      "listed-1",
      "listed-2",
      "listed-3",
      "listed-4",
      "listed-5",
    ]);
    expect(remainderCount).toBe(1);
    expect(totalCount).toBe(8); // excludes 'broken'
  });

  it("caps recent cards on narrow screens", () => {
    const now = 10000000;
    const items: ArchivedSession[] = [
      session("s1", 5 * 60 * 1000),
      session("s2", 10 * 60 * 1000),
      session("s3", 15 * 60 * 1000),
    ];
    const { recentCards, listedRows } = partitionArchived(items, now, true); // narrow
    // Narrow caps recent cards at 1 or 2
    expect(recentCards.length).toBeLessThanOrEqual(2);
    expect(recentCards.length + listedRows.length).toBe(3);
  });
});

describe("filterArchived", () => {
  const items: ArchivedSession[] = [
    {
      id: "eager-kestrel",
      cwd: "/repo",
      title: "Designing web client composer",
      state: "archived",
    },
    {
      id: "spare-otter",
      cwd: "/repo",
      title: "On-call requests",
      state: "archived",
    },
    {
      id: "mild-otter",
      cwd: "/repo",
      title: "Fixing tests",
      state: "archived",
    },
  ];

  it("matches all words across title and id", () => {
    expect(filterArchived(items, "").length).toBe(3);
    expect(filterArchived(items, "composer").map((s) => s.id)).toEqual([
      "eager-kestrel",
    ]);
    expect(filterArchived(items, "otter").map((s) => s.id)).toEqual([
      "spare-otter",
      "mild-otter",
    ]);
    expect(filterArchived(items, "otter fixing").map((s) => s.id)).toEqual([
      "mild-otter",
    ]);
  });
});
