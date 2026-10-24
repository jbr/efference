import { useEffect, useRef, useState, type ReactNode } from "react";
import { Markdown } from "./Markdown.tsx";

/** A box of fixed height pinned to its newest line: a long body scrolls
 * inside itself instead of pushing the page. The top fades only while lines
 * are scrolled out above it; a body that never outgrows the box has nothing
 * hidden and no fade. `pinned` is false once the body stops growing — the box
 * then holds where the reader left it. The live thinking tail, a committed
 * long think in the recent tier, and a running command's output are all this
 * box, so an item that finishes streaming does not change shape. */
export function PinnedBox({
  children,
  grows,
  className,
}: {
  children: ReactNode;
  /** Changes whenever the body grows; the box re-pins to the bottom. */
  grows: unknown;
  className?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [clipped, setClipped] = useState(false);
  useEffect(() => {
    const element = box.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
    setClipped(element.scrollTop > 0);
  }, [grows]);
  return (
    <div
      ref={box}
      className={["t-pinned", clipped && "clipped", className]
        .filter(Boolean)
        .join(" ")}
      onScroll={(event) => setClipped(event.currentTarget.scrollTop > 0)}
    >
      {children}
    </div>
  );
}

/** The live thinking tail, in the box it will stay in once committed. */
export function LiveThinking({ text }: { text: string }) {
  return (
    <div className="t-entry">
      <div className="t-box-head">Thinking…</div>
      <PinnedBox grows={text} className="t-thinking">
        <Markdown>{text}</Markdown>
      </PinnedBox>
    </div>
  );
}
