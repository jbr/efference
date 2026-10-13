import { useId, type ReactNode } from "react";
import type { Usage } from "../api/types.ts";
import { FoldControl, type FoldControlProps } from "./FoldControl.tsx";
import { HoverPopup } from "./HoverPopup.tsx";
import "./ContextPopup.css";

export function ContextPopup({
  context,
  summary,
  children,
}: {
  context: Usage["context"];
  summary: FoldControlProps;
  children?: ReactNode;
}) {
  const id = useId();
  return (
    <HoverPopup
      id={id}
      role="dialog"
      labelledBy={`${id}-title`}
      className="context-popup"
      renderTrigger={(open, show) => (
        <button
          type="button"
          className="context-popup-trigger"
          aria-label="Context and summarize history"
          aria-expanded={open}
          aria-controls={id}
          onClick={show}
        >
          {children ?? "Context"}
        </button>
      )}
    >
      <h2 id={`${id}-title`} className="hover-popup-title">
        Context
      </h2>
      <p>
        {context?.used == null
          ? "Context usage is not available yet."
          : context.limit == null
            ? `${context.used.toLocaleString()} tokens in the last request; window size unknown.`
            : `${context.used.toLocaleString()} of ${context.limit.toLocaleString()} tokens in the last request.`}
      </p>
      <FoldControl {...summary} />
    </HoverPopup>
  );
}
