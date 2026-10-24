import type { CurrentOperations, LogEntry } from "../api/types.ts";
import type { Streaming } from "./store.ts";
import type { SummaryPreviewNode } from "./tree.ts";

/** Live accounts use the same placement as their eventual summaries (docs/web-client.md). */
export function summaryPreviews(
  entries: LogEntry[],
  streaming: Streaming,
  operations: CurrentOperations | null,
): SummaryPreviewNode[] {
  const previews: SummaryPreviewNode[] = [];
  const requested = entries.findLast(
    (entry) =>
      "Telemetry" in entry.payload &&
      "CompactionRequested" in entry.payload.Telemetry,
  )?.payload;
  const cut =
    operations?.compaction?.cut ??
    (requested &&
    "Telemetry" in requested &&
    "CompactionRequested" in requested.Telemetry
      ? requested.Telemetry.CompactionRequested.cut
      : undefined);
  if (streaming.compaction && cut !== undefined)
    previews.push({
      kind: "summary-preview",
      key: "live-compaction",
      source: "compaction",
      from: 0,
      upto: cut,
      ...streaming.compaction,
    });
  if (streaming.consolidation)
    previews.push({
      kind: "summary-preview",
      key: "live-consolidation",
      source: "consolidation",
      ...streaming.consolidation,
    });
  return previews;
}
