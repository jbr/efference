// The model picker's rows (docs/web-client.md, docs/tui.md): what the list
// shows for a path, which the chips and the role decide rather than the query.
// Pure, so the rules are testable without a host; the ordering *within* a query
// is `matcher.ts`'s.
//
// The rules are the terminal form's (docs/tui.md). With nothing typed the root
// lists, for the main role, the top few recent picks across connections and
// then every connection; the other two roles list the connections alone, since
// history records only main picks. Under a connection chip the main role lists
// that connection's recents first, then its groups by name — or, for a provider
// without groups, its models in catalog order. Under a group chip there is only
// its models. A query filters all of that through the matcher.

import type { CatalogEntry, CatalogModel } from "../api/sessionForm.ts";
import { type Chip, type Node, id, isInterior, rank } from "./matcher.ts";

/** Which role is choosing. Only main's picks are recorded, so only main's list
 * leads with recents. */
export type Role = "main" | "spotter" | "triage";

/** How many recent picks lead a list. */
const RECENTS = 5;

/** One row of the picker: a connection or group to descend into, a model to
 * pick, or the typed-id row. */
export type Row =
  | { kind: "connection"; node: Node; entry: CatalogEntry }
  | { kind: "group"; node: Node; count: number }
  | { kind: "model"; node: Node; model: CatalogModel; recent: boolean }
  | { kind: "typed"; connection: string; model: string };

/** The catalog as the picker's flat node list: a connection per entry, a group
 * per author its models carry, and the models themselves. `recent` is the rank
 * the host already folded in, so the ranking and the rows cannot disagree. */
export function catalogNodes(entries: CatalogEntry[]): Node[] {
  const nodes: Node[] = [];
  for (const entry of entries) {
    nodes.push({ kind: "connection", connection: entry.connection });
    const groups = new Set(
      entry.models.flatMap((model) =>
        model.group === undefined ? [] : [model.group],
      ),
    );
    for (const group of [...groups].sort()) {
      nodes.push({ kind: "group", connection: entry.connection, group });
    }
    for (const model of entry.models) {
      nodes.push({
        kind: "model",
        connection: entry.connection,
        ...(model.group === undefined ? {} : { group: model.group }),
        model: model.model,
        ...(model.name === undefined ? {} : { name: model.name }),
        ...(model.recent === undefined ? {} : { recent: model.recent }),
      });
    }
  }
  return nodes;
}

/** The rows for a path, a query and a role.
 *
 * With a query, every visible row is the matcher's ordering and the typed-id
 * row follows it. Without one, the recents lead and the rest follow, each taken
 * once — the list is the index's, not a union. */
export function rows(
  nodes: Node[],
  entries: CatalogEntry[],
  chips: Chip[],
  query: string,
  role: Role,
): Row[] {
  // Recents are the matcher's own tie-break, so ranking first is what puts the
  // best picks first for the main role; the other roles do not count them.
  const ranked = rank(nodes, chips, query, role === "main").map(
    (index) => nodes[index],
  );

  if (query.trim() !== "") {
    return [
      ...ranked.map((node) => rowFor(node, entries)).filter(notNull),
      ...typedIdRow(chips, query),
    ];
  }

  // Recents lead the list, taken across every connection at the root, for the
  // one role whose picks are recorded.
  const leads =
    role === "main"
      ? ranked
          .filter((node) => !isInterior(node) && node.recent !== undefined)
          .slice(0, RECENTS)
      : [];
  // At an untouched root the rest of the list is the connections: a group's
  // models are found by descending into it, and offering every group's rows
  // here would bury them.
  const rest =
    chips.length === 0
      ? ranked.filter((node) => node.kind === "connection")
      : ranked;
  const shown = new Set(leads);
  return [
    ...leads.map((node) => rowFor(node, entries)).filter(notNull),
    ...rest
      .filter((node) => !shown.has(node))
      .map((node) => rowFor(node, entries))
      .filter(notNull),
  ];
}

/** The typed-id row: *use `<typed>` as model id*. Under a group chip the bare
 * id is prefixed with the group, since the chip already names the author. It is
 * offered whenever the query is one word and a chip is in — its presence says
 * nothing about whether anything matched, because a wanted id can be a prefix
 * of a listed one. */
export function typedIdRow(chips: Chip[], query: string): Row[] {
  const typed = query.trim();
  if (chips.length === 0 || typed === "" || /\s/.test(typed)) return [];
  const [connection, group] = chips;
  return [
    {
      kind: "typed",
      connection,
      model: group === undefined ? typed : `${group}/${typed}`,
    },
  ];
}

const notNull = <T>(row: T | null): row is T => row !== null;

function rowFor(node: Node, entries: CatalogEntry[]): Row | null {
  if (node.kind === "connection") {
    const entry = entries.find((one) => one.connection === node.connection);
    return entry === undefined ? null : { kind: "connection", node, entry };
  }
  if (node.kind === "group") {
    return {
      kind: "group",
      node,
      count: modelsIn(entries, node.connection, node.group).length,
    };
  }
  const model = modelsIn(entries, node.connection, node.group).find(
    (one) => one.model === node.model,
  );
  return model === undefined
    ? null
    : { kind: "model", node, model, recent: node.recent !== undefined };
}

const modelsIn = (
  entries: CatalogEntry[],
  connection: string,
  group: string | undefined,
): CatalogModel[] =>
  entries
    .filter((entry) => entry.connection === connection)
    .flatMap((entry) => entry.models)
    .filter((model) => model.group === group);

/** The row's id as the picker shows it. */
export const rowSlug = (row: Row): string =>
  row.kind === "typed" ? `${row.connection}/${row.model}` : id(row.node);

/** Whether a row is descended into rather than picked. */
export const descends = (row: Row): boolean =>
  row.kind === "connection" || row.kind === "group";

/** A model's name shows only when it says something the id does not: the
 * lowercased alphanumerics differing from the id's last segment. */
export function nameAdds(node: Node, name: string | undefined): boolean {
  if (name === undefined) return false;
  const last = (node.model ?? "").split("/").pop() ?? "";
  const alnum = (text: string) => text.toLowerCase().replace(/[^a-z0-9]/g, "");
  return alnum(name) !== alnum(last);
}

/** The context window as `1M` / `272k`, absent when the source did not say. */
export const contextFact = (model: CatalogModel): string | null =>
  model.context === undefined ? null : context(model.context);

const context = (tokens: number): string => {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  // Whole thousands, rounded by truncating rather than by `Math.round`, which
  // would send a half up where the terminal form sends it down.
  if (tokens >= 1_000) return `${Math.floor((tokens + 500) / 1_000)}k`;
  return `${tokens}`;
};

/** The price as `$0.15/$0.50`, or `free` when both sides are zero. USD per
 * million input and output tokens; a side under a cent keeps a third digit
 * rather than rounding itself away. */
export function priceFact(model: CatalogModel): string | null {
  const price = model.price;
  if (price === undefined) return null;
  if (price.input === 0 && price.output === 0) return "free";
  const usd = (value: number) =>
    value > 0 && value < 0.01 ? `$${value.toFixed(3)}` : `$${value.toFixed(2)}`;
  return `${usd(price.input)}/${usd(price.output)}`;
}

/** The model's age as `3d` / `5w` / `8mo` / `2y`, absent when the source
 * declared none. A `created` in the future reads as today rather than as a
 * negative age. */
export function ageFact(model: CatalogModel, now: number): string | null {
  if (model.created === undefined) return null;
  const days = Math.max(0, Math.floor((now - model.created) / 86_400));
  if (days < 7) return `${days}d`;
  if (days < 60) return `${Math.floor(days / 7)}w`;
  if (days < 730) return `${Math.floor(days / 30)}mo`;
  return `${Math.floor(days / 365)}y`;
}
