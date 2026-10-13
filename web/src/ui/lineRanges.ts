export type LineRange = readonly [number, number];
export function mergeRanges(
  ranges: readonly LineRange[],
  count: number,
): LineRange[] {
  const sorted = ranges
    .map(([a, b]) => [Math.max(1, a), Math.min(count, b)] as const)
    .filter(([a, b]) => a <= b)
    .sort((a, b) => a[0] - b[0]);
  const result: [number, number][] = [];
  for (const [a, b] of sorted) {
    const last = result.at(-1);
    if (last && a <= last[1] + 1) last[1] = Math.max(last[1], b);
    else result.push([a, b]);
  }
  return result;
}
