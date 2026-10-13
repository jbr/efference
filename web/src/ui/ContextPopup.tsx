import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Usage } from "../api/types.ts";
import { FoldControl, type FoldControlProps } from "./FoldControl.tsx";
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
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const position = useCallback(() => {
    const anchor = button.current?.getBoundingClientRect();
    if (anchor && panel.current)
      panel.current.style.top = `${anchor.bottom + 8}px`;
  }, []);
  useEffect(() => {
    if (!open) return;
    position();
    window.addEventListener("resize", position);
    window.addEventListener("scroll", position, true);
    return () => {
      window.removeEventListener("resize", position);
      window.removeEventListener("scroll", position, true);
    };
  }, [open, position]);
  return (
    <>
      <button
        ref={button}
        type="button"
        className="context-popup-trigger"
        aria-label="Context and summarize history"
        aria-expanded={open}
        aria-controls={id}
        popoverTarget={id}
        onClick={position}
      >
        {children ?? "Context"}
      </button>
      <div
        ref={panel}
        id={id}
        popover="auto"
        className="context-popup"
        role="dialog"
        aria-labelledby={`${id}-title`}
        onToggle={(event) =>
          setOpen((event.nativeEvent as ToggleEvent).newState === "open")
        }
      >
        <div className="context-popup-heading">
          <h2 id={`${id}-title`}>Context</h2>
          <button
            type="button"
            className="quiet"
            popoverTarget={id}
            popoverTargetAction="hide"
            aria-label="Close context details"
          >
            Close
          </button>
        </div>
        <p>
          {context?.used == null
            ? "Context usage is not available yet."
            : context.limit == null
              ? `${context.used.toLocaleString()} tokens in the last request; window size unknown.`
              : `${context.used.toLocaleString()} of ${context.limit.toLocaleString()} tokens in the last request.`}
        </p>
        <FoldControl {...summary} />
      </div>
    </>
  );
}
