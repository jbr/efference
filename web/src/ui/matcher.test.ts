// The matching rule against the fixture both implementations run
// (`src/clients/tui/session_form/match_cases.json`), read from that path rather than
// copied: the fixture is the contract, and a copy would let the two drift
// without either noticing. `src/clients/tui/session_form/matcher/test.rs` runs the same
// cases the same way.

import { expect, test } from "vitest";
import { type Node, id, rank, stripAuthor } from "./matcher.ts";
// The Rust module's own fixture, imported from that path rather than copied: the
// file is the contract both implementations run, and a copy would let them
// drift without either noticing. Vite resolves it the way it resolves any JSON
// import — read from disk on every run.
import raw from "../../../src/clients/tui/session_form/match_cases.json";

interface Case {
  why: string;
  query: string;
  chips: string[];
  role: string;
  expect: string[];
}

const fixture = raw as unknown as { tree: Node[]; cases: Case[] };

test("the fixture is read from the Rust module's own path", () => {
  expect(fixture.tree.length).toBeGreaterThan(0);
  expect(fixture.cases.length).toBeGreaterThan(0);
});

test.each(fixture.cases.map((c) => [c.why, c] as const))("%s", (_why, item) => {
  const got = rank(
    fixture.tree,
    item.chips,
    item.query,
    item.role === "main",
  ).map((index) => id(fixture.tree[index]));
  expect(got, `${item.why} (query ${JSON.stringify(item.query)})`).toEqual(
    item.expect,
  );
});

test("an alias marker is part of the id, not a prefix to drop", () => {
  // `~moonshotai/kimi-latest` and `moonshotai/kimi-latest` are different ids, so
  // the marker survives the author prefix being stripped.
  expect(stripAuthor("~moonshotai/kimi-latest")).toBe("~kimi-latest");
  expect(stripAuthor("z-ai/glm-5.3")).toBe("glm-5.3");
  expect(stripAuthor("gpt-5.6")).toBe("gpt-5.6");
});

test("an empty query ranks every row in scope, interior before leaves", () => {
  // The picker's untouched root: what it lists before anything is typed. Every
  // row ties on tier, so the ordering is interior first, then — because recents
  // count for main — the recent picks, then list order.
  const all = rank(fixture.tree, [], "", true).map((i) => id(fixture.tree[i]));
  expect(all).toHaveLength(fixture.tree.length);
  // The first four are the connections and the three groups, in list order.
  expect(all.slice(0, 4)).toEqual([
    "router",
    "router/anthropic",
    "router/moonshotai",
    "router/z-ai",
  ]);
  // Then the leaves, the recent picks first.
  expect(all.slice(6, 9)).toEqual([
    "lab/gpt-5.6",
    "router/z-ai/glm-5.3",
    "work/claude-sonnet-4-5",
  ]);
});
