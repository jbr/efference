import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  orderByRecency,
  orderByTabs,
  readVisits,
  recordVisit,
} from "./switcherOrder.ts";

const ids = (sessions: { id: string }[]) => sessions.map((s) => s.id);
const listed = ["a", "b", "c", "d"].map((id) => ({ id }));

describe("orderByRecency", () => {
  it("keeps the listing's order when nothing was visited", () => {
    expect(ids(orderByRecency(listed, {}))).toEqual(["a", "b", "c", "d"]);
  });

  it("puts visited sessions first, newest first, then the rest as listed", () => {
    const visits = { c: 30, a: 10 };
    expect(ids(orderByRecency(listed, visits))).toEqual(["c", "a", "b", "d"]);
  });

  it("ignores visits to sessions that are not listed", () => {
    expect(ids(orderByRecency(listed, { gone: 99, b: 1 }))).toEqual([
      "b",
      "a",
      "c",
      "d",
    ]);
  });
});

describe("visits in storage", () => {
  beforeEach(() => {
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("records and reads back a visit", () => {
    recordVisit("x", 5);
    recordVisit("y", 9);
    expect(readVisits()).toEqual({ x: 5, y: 9 });
  });

  it("keeps only the newest hundred", () => {
    for (let i = 0; i < 120; i++) recordVisit(`s${i}`, i);
    const visits = readVisits();
    expect(Object.keys(visits)).toHaveLength(100);
    expect(visits.s119).toBe(119);
    expect(visits.s0).toBeUndefined();
  });

  it("reads corrupt storage as no visits", () => {
    localStorage.setItem("efference.sessionVisits", "not json");
    expect(readVisits()).toEqual({});
  });
});

describe("orderByTabs", () => {
  it("preserves slots across changed visits and listing order", () => {
    const previous = ["b", "a", "c"];
    expect(
      ids(orderByTabs([{ id: "c" }, { id: "a" }, { id: "b" }], previous)),
    ).toEqual(previous);
  });
  it("removes absent sessions and appends new sessions", () => {
    expect(
      ids(
        orderByTabs([{ id: "d" }, { id: "b" }, { id: "a" }], ["a", "b", "c"]),
      ),
    ).toEqual(["a", "b", "d"]);
  });
});
