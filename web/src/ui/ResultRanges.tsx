import { memo } from "react";
import type { RawResult, WireState } from "../api/types.ts";
import { rustLines } from "../render/triage.ts";
import { diffRows, readRows, isDiffTool } from "../render/parse.ts";
import { RangeContent } from "./RangeContent.tsx";
import { mergeRanges } from "./lineRanges.ts";

/** Transcript expansion stays local; raw coordinates remain the verdict's
 * coordinates even when tool framing is removed (docs/web-client.md). */
export const ResultRanges = memo(function ResultRanges({
  tool,
  raw,
  wire,
  lang,
}: {
  tool: string | null;
  raw: RawResult;
  wire: WireState;
  lang: string | null;
}) {
  const diff =
    !raw.is_error && tool !== null && isDiffTool(tool)
      ? diffRows(raw.content)
      : null;
  const read = !raw.is_error && tool === "read" ? readRows(raw.content) : null;
  const rows: [number, string][] = diff
    ? diff.map(([n, l]) => [n, l.text])
    : (read?.lines ?? rustLines(raw.content).map((line, i) => [i + 1, line]));
  const keep =
    typeof wire === "object" && "Triaged" in wire ? wire.Triaged.keep : "All";
  const selected = rows.flatMap(([n], i) =>
    keep === "All" ||
    (keep !== "None" && keep.Ranges.some(([a, b]) => n >= a && n <= b))
      ? [i + 1]
      : [],
  );
  const retained = mergeRanges(
    selected.map((n) => [n, n]),
    rows.length,
  );
  const initial =
    wire === "Pending" || selected.length <= 20
      ? retained
      : mergeRanges(
          selected.slice(0, 10).map((n) => [n, n]),
          rows.length,
        );
  const classes = new Map(diff?.map(([n, line]) => [n, `diff-${line.kind}`]));
  return (
    <RangeContent
      variant="transcript"
      text={rows.length ? rows.map(([, line]) => line).join("\n") + "\n" : ""}
      numbers={rows.map(([n]) => n)}
      retained={retained}
      initial={initial}
      lang={read?.lang ?? lang}
      lineClass={(n) => classes.get(n)}
    />
  );
});
