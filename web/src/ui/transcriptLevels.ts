// How much of each transcript item renders (docs/web-client.md, "Detail by
// recency"): the rules, pure, so the component holds none of them.
//
// The transcript is a run of items — a user turn, a paragraph run, a thinking
// block, one call. A batch (the calls one generation issued) is flattened:
// two calls read as two steps. Each item gets a level by its distance from
// the tail: recent items render whole, the next stretch trimmed to a glance,
// and older items as one-line summaries. Nothing is dropped — every level
// opens to the whole on a tap.

import type { CallNode, LeafNode, TranscriptNode } from "../session/tree.ts";

export type Level = "full" | "trimmed" | "brief";

/** Items this close to the tail render whole. */
export const RECENT = 6;
/** Items this far from the tail and beyond render brief. */
export const ANCIENT = 20;
/** Thinking at or under this many words is never folded — about 8 wrapped
 * lines, the median block (backlog/web-thinking-autocollapse.md), where a
 * fold costs a click and saves nothing. Words rather than newlines (thinking
 * is a few long paragraphs) or characters (markdown syntax inflates them). */
export const THINKING_FLOOR = 120;

export type Item =
  { kind: "leaf"; node: LeafNode } | { kind: "call"; node: CallNode };

export function flatten(nodes: TranscriptNode[]): Item[] {
  const items: Item[] = [];
  for (const node of nodes) {
    if (node.kind === "batch") {
      for (const call of node.calls) items.push({ kind: "call", node: call });
    } else {
      items.push({ kind: "leaf", node });
    }
  }
  return items;
}

export const isThinking = (node: LeafNode) => node.key.endsWith(":thinking");

export const wordCount = (text: string) =>
  text.split(/\s+/).filter((word) => word !== "").length;

/** A call that renders whole at any distance: still running or held at a
 * gate, failed, withheld by a triage failure, or flagged by the spotter.
 * Errors, gates, and refusals render at every density
 * (backlog/web-transcript-density.md). */
export function mustShow(node: CallNode): boolean {
  if (node.result === null) return true;
  const payload = node.result.payload;
  if ("ToolResult" in payload) {
    const { raw, wire } = payload.ToolResult;
    if (raw.is_error || (typeof wire === "object" && "TriageFailed" in wire))
      return true;
  }
  const stance = node.spot?.stance;
  return stance === "gate" || stance === "watch";
}

/** An item's level, `fromEnd` items from the tail (0 is the last). */
export function levelFor(item: Item, fromEnd: number): Level {
  if (item.kind === "call" && mustShow(item.node)) return "full";
  return fromEnd < RECENT ? "full" : fromEnd < ANCIENT ? "trimmed" : "brief";
}
