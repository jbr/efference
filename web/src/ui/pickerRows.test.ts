// The picker's rows (docs/web-client.md): which rows a path shows, the
// typed-id row, and the facts beside a model. Pure over the catalog the host
// served; the ordering within a query is `matcher.ts`'s, which the TUI's fixture
// covers.

import { expect, test } from "vitest";
import type { CatalogEntry } from "../api/sessionForm.ts";
import {
  ageFact,
  catalogNodes,
  contextFact,
  priceFact,
  rowSlug,
  rows,
  typedIdRow,
} from "./pickerRows.ts";

const entry = (over: Partial<CatalogEntry> = {}): CatalogEntry => ({
  connection: "router",
  provider: "openrouter",
  status: "fresh",
  models: [],
  ...over,
});

const router = entry({
  models: [
    { model: "z-ai/glm-5.3", group: "z-ai", name: "GLM 5.3", recent: 1 },
    { model: "z-ai/glm-5.3-flash", group: "z-ai" },
    { model: "anthropic/claude-sonnet-4.5", group: "anthropic" },
  ],
});
const lab = entry({
  connection: "lab",
  provider: "typesafe",
  models: [{ model: "gpt-5.6", name: "GPT-5.6", recent: 2 }],
});
const entries = [router, lab];
const nodes = catalogNodes(entries);

const slugs = (list: ReturnType<typeof rows>) => list.map(rowSlug);

test("the catalog becomes connections, their groups, and their models", () => {
  expect(nodes[0]).toEqual({ kind: "connection", connection: "router" });
  // Groups are interior nodes, sorted, ahead of the models that carry them.
  expect(
    nodes
      .filter((node) => node.kind === "group")
      .map((node) => rowSlug({ kind: "group", node, count: 0 })),
  ).toEqual(["router/anthropic", "router/z-ai"]);
  expect(nodes.findIndex((node) => node.kind === "group")).toBe(1);
  expect(
    nodes.filter((node) => node.kind === "model").map((node) => node.model),
  ).toEqual([
    "z-ai/glm-5.3",
    "z-ai/glm-5.3-flash",
    "anthropic/claude-sonnet-4.5",
    "gpt-5.6",
  ]);
  // A provider without groups contributes no group node.
  expect(
    nodes.filter((node) => node.kind === "group" && node.connection === "lab"),
  ).toHaveLength(0);
});

test("an untouched root leads with recents for main, and with connections alone for the others", () => {
  const main = rows(nodes, entries, [], "", "main");
  expect(slugs(main).slice(0, 2)).toEqual([
    "router/z-ai/glm-5.3",
    "lab/gpt-5.6",
  ]);
  // Every connection is listed after them, and each one once.
  expect(main.filter((row) => row.kind === "connection").map(rowSlug)).toEqual([
    "router",
    "lab",
  ]);

  // History records only main picks, so the other roles start at the
  // connections and list no recent at all.
  const spotter = rows(nodes, entries, [], "", "spotter");
  expect(spotter.map(rowSlug)).toEqual(["router", "lab"]);
});

test("a recency leads the list only once", () => {
  const main = rows(nodes, entries, [], "", "main");
  expect(slugs(main).filter((slug) => slug === "router/z-ai/glm-5.3")).toEqual([
    "router/z-ai/glm-5.3",
  ]);
});

test("under a connection chip the main role sees its recents, then its groups", () => {
  const main = rows(nodes, entries, ["router"], "", "main");
  expect(slugs(main)).toEqual([
    "router/z-ai/glm-5.3",
    "router/anthropic",
    "router/z-ai",
    "router/z-ai/glm-5.3-flash",
    "router/anthropic/claude-sonnet-4.5",
  ]);
  // A provider without groups lists its models where another lists groups.
  expect(slugs(rows(nodes, entries, ["lab"], "", "spotter"))).toEqual([
    "lab/gpt-5.6",
  ]);
});

test("under a group chip there is only its models", () => {
  expect(slugs(rows(nodes, entries, ["router", "z-ai"], "", "main"))).toEqual([
    "router/z-ai/glm-5.3",
    "router/z-ai/glm-5.3-flash",
  ]);
});

test("a query filters through the matcher, and the typed-id row follows it", () => {
  expect(slugs(rows(nodes, entries, [], "glm", "main"))).toEqual([
    "router/z-ai/glm-5.3",
    "router/z-ai/glm-5.3-flash",
  ]);
  // Under a connection chip a one-word query also offers the typed id, whether
  // or not something matched — a wanted id can be a prefix of a listed one.
  expect(slugs(rows(nodes, entries, ["router"], "zzz", "main"))).toEqual([
    "router/zzz",
  ]);
  expect(slugs(rows(nodes, entries, ["router"], "glm", "main")).at(-1)).toBe(
    "router/glm",
  );
  // At the root there is no connection to type an id into.
  expect(typedIdRow([], "glm")).toEqual([]);
  // Two words is not one id.
  expect(typedIdRow(["router"], "glm 5")).toEqual([]);
});

test("a typed id under a group chip carries the group", () => {
  // The chip already names the author, so the bare id is completed with it.
  expect(typedIdRow(["router", "z-ai"], "glm-9")).toEqual([
    { kind: "typed", connection: "router", model: "z-ai/glm-9" },
  ]);
});

test("a model's facts are what the source declared, and nothing more", () => {
  const model = {
    model: "m",
    context: 272_000,
    price: { input: 0.15, output: 0.5 },
    created: 1_700_000_000,
  };
  expect(contextFact(model)).toBe("272k");
  expect(contextFact({ model: "m", context: 1_000_000 })).toBe("1.0M");
  expect(priceFact(model)).toBe("$0.15/$0.50");
  expect(priceFact({ model: "m", price: { input: 0, output: 0 } })).toBe(
    "free",
  );
  // A side under a cent keeps a third digit rather than rounding to nothing.
  expect(priceFact({ model: "m", price: { input: 0.005, output: 1 } })).toBe(
    "$0.005/$1.00",
  );

  // The scale is the terminal form's: whole days and whole units, with a date
  // in the future reading as today rather than as a negative age.
  const now = model.created + 3 * 86_400;
  expect(ageFact(model, now)).toBe("3d");
  expect(ageFact(model, model.created + 5 * 7 * 86_400)).toBe("5w");
  expect(ageFact(model, model.created + 8 * 30 * 86_400)).toBe("8mo");
  expect(ageFact(model, model.created + 2 * 365 * 86_400)).toBe("2y");
  expect(ageFact(model, model.created - 100)).toBe("0d");
  // Undeclared is absent, not zero.
  expect(contextFact({ model: "m" })).toBe(null);
  expect(priceFact({ model: "m" })).toBe(null);
  expect(ageFact({ model: "m" }, now)).toBe(null);
});
