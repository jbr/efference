import { useLayoutEffect, useRef, useState } from "react";

import { ambientText } from "./ambientText.ts";

export function Ambient({ source, body }: { source: string; body: string }) {
  const [expanded, setExpanded] = useState(false);
  const [overflow, setOverflow] = useState(false);
  const preview = useRef<HTMLDivElement>(null);
  const text = ambientText(body);
  useLayoutEffect(() => {
    const element = preview.current;
    if (!element || expanded) return;
    const measure = () =>
      setOverflow(element.scrollHeight > element.clientHeight + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [expanded, text, source]);
  return (
    <div className="ambient-notice">
      <div
        ref={preview}
        className={`ambient-preview${expanded ? " expanded" : ""}`}
      >
        <span className="ambient-source">{ambientText(source)}</span> — {text}
      </div>
      {(overflow || expanded) && (
        <button
          className="quiet ambient-toggle"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
    </div>
  );
}
