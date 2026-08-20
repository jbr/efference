// The strip under the header (docs/web-client.md): what is left of the
// original controls strip after its redistribution — the usage line, which
// moves to the top bar when that unit lands. The loop dials live on the
// composer's status row; process and notification rows live in the bottom
// panel.

import type { Usage } from "../api/types.ts";

const tokens = (n: number) =>
  n >= 1000 ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k` : `${n}`;

/** Context occupancy + session cost, whichever halves the provider filled. */
function usageLine(usage: Usage): string {
  const parts: string[] = [];
  if (usage.context) {
    const used = usage.context.used === null ? "?" : tokens(usage.context.used);
    parts.push(`ctx ${used}/${tokens(usage.context.limit)}`);
  }
  if (usage.cost) {
    const unpriced =
      usage.cost.unpriced > 0 ? ` (+${usage.cost.unpriced} unpriced)` : "";
    parts.push(`$${usage.cost.usd.toFixed(4)}${unpriced}`);
  }
  return parts.join(" · ");
}

export function Controls({ usage }: { usage: Usage | null }) {
  if (usage === null) return null;
  return <div className="controls-strip dim">{usageLine(usage)}</div>;
}
