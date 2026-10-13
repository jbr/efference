import { useLayoutEffect, useRef, useState } from "react";
import { fitTabs } from "./switcherOrder.ts";

/** Read natural tab widths from the hidden strip (docs/web-client.md). */
export function useSwitcherFit(labels: string[], currentId: string) {
  const row = useRef<HTMLElement>(null);
  const measure = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState<Set<string> | null>(null);
  const key = labels.join("\n");
  useLayoutEffect(() => {
    const element = row.current;
    const strip = measure.current;
    if (element === null || strip === null) return;
    const ids = key.split("\n");
    const compute = () => {
      const style = getComputedStyle(element);
      const gap = parseFloat(getComputedStyle(strip).columnGap) || 0;
      const room =
        element.clientWidth -
        parseFloat(style.paddingLeft) -
        parseFloat(style.paddingRight);
      const widths = [...strip.children].map(
        (child) => child.getBoundingClientRect().width,
      );
      const more = widths.pop() ?? 0;
      setFit(
        fitTabs(
          ids.map((id, index) => ({ id, width: widths[index] ?? 0 })),
          currentId,
          room,
          more,
          gap,
        ),
      );
    };
    compute();
    const observer = new ResizeObserver(compute);
    observer.observe(element);
    return () => observer.disconnect();
  }, [key, currentId]);
  return { row, measure, fit };
}
