// Namespace browsing and search rows (docs/web-session-creation.md).
// Browsing exposes direct children, with main-role recents allowed to lead.
// Query ranking is matcher.ts, pinned to the terminal match fixture.

import type { CatalogModel, ConnectionCatalog } from "../api/types.ts";
import { type Chip, type Node, id, isInterior, rank } from "./matcher.ts";

/** Which role is choosing. Only main's picks are recorded, so only main's list
 * leads with recents. */
export type Role = "main" | "spotter" | "triage";

/** How many recent picks lead a list. */
const RECENTS = 5;

/** One row of the picker: a connection or group to descend into, a model to
 * pick, or the typed-id row. */
export type Row =
  | { kind: "connection"; node: Node; entry: ConnectionCatalog }
  | { kind: "group"; node: Node; count: number }
  | { kind: "model"; node: Node; model: CatalogModel; recent: boolean }
  | { kind: "typed"; connection: string; model: string };

/** The catalog as the picker's flat node list: a connection per entry, a group
 * per author its models carry, and the models themselves. `recent` is the rank
 * the host already folded in, so the ranking and the rows cannot disagree. */
export function catalogNodes(entries: ConnectionCatalog[]): Node[] {
  const nodes: Node[] = [];
  for (const entry of entries) {
    nodes.push({ kind: "connection", connection: entry.connection });
    const groups = new Set(
      entry.models.flatMap((model) =>
        model.group == null ? [] : [model.group],
      ),
    );
    for (const group of [...groups].sort()) {
      nodes.push({ kind: "group", connection: entry.connection, group });
    }
    for (const model of entry.models) {
      nodes.push({
        kind: "model",
        connection: entry.connection,
        ...(model.group == null ? {} : { group: model.group }),
        model: model.model,
        ...(model.name == null ? {} : { name: model.name }),
        ...(model.recent == null ? {} : { recent: model.recent }),
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
  entries: ConnectionCatalog[],
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
  const rest = ranked.filter((node) => {
    if (chips.length === 0) return node.kind === "connection";
    if (chips.length === 1) {
      return (
        node.kind === "group" ||
        (node.kind === "model" && node.group === undefined)
      );
    }
    return true;
  });
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

function rowFor(node: Node, entries: ConnectionCatalog[]): Row | null {
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
  entries: ConnectionCatalog[],
  connection: string,
  group: string | undefined,
): CatalogModel[] =>
  entries
    .filter((entry) => entry.connection === connection)
    .flatMap((entry) => entry.models)
    .filter((model) => (model.group ?? undefined) === group);

/** The row's id as the picker shows it. */
export const rowSlug = (row: Row): string =>
  row.kind === "typed" ? `${row.connection}/${row.model}` : id(row.node);

/** A typed id may equal a catalog slug while offering a different action. */
export const rowKey = (row: Row): string => `${row.kind}:${rowSlug(row)}`;

/** Whether a row is descended into rather than picked. */
export const descends = (row: Row): boolean =>
  row.kind === "connection" || row.kind === "group";

/** The model's age as `3d` / `5w` / `8mo` / `2y`, absent when the source
 * declared none. A `created` in the future reads as today rather than as a
 * negative age. */
export function ageFact(model: CatalogModel, now: number): string | null {
  if (model.created == null || model.created <= 0) return null;
  const days = Math.max(0, Math.floor((now - model.created) / 86_400));
  if (days < 7) return `${days}d`;
  if (days < 60) return `${Math.floor(days / 7)}w`;
  if (days < 730) return `${Math.floor(days / 30)}mo`;
  return `${Math.floor(days / 365)}y`;
}
