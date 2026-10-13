import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type MouseEvent,
} from "react";
import { useKeepOnScreen } from "./useKeepOnScreen.ts";
import "./HoverPopup.css";

export const HOVER_OPEN_DELAY_MS = 180;
export const HOVER_CLOSE_DELAY_MS = 120;

/** Anchored details with a pointer bridge and keyboard dismissal (docs/web-client.md). */
export function HoverPopup({
  children,
  renderTrigger,
  className,
  id,
  role,
  labelledBy,
}: {
  children: ReactNode;
  renderTrigger: (
    open: boolean,
    show: (event: MouseEvent) => void,
  ) => ReactNode;
  className?: string;
  id: string;
  role: "tooltip" | "dialog";
  labelledBy?: string;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLSpanElement>(null);
  const panel = useKeepOnScreen(open);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const keyboard = useRef(true);
  const show = useCallback((event: MouseEvent) => {
    if (event.detail === 0) setOpen(true);
  }, []);
  useEffect(() => {
    const key = () => {
      keyboard.current = true;
    };
    const pointer = () => {
      keyboard.current = false;
    };
    document.addEventListener("keydown", key, true);
    document.addEventListener("pointerdown", pointer, true);
    return () => {
      document.removeEventListener("keydown", key, true);
      document.removeEventListener("pointerdown", pointer, true);
    };
  }, []);
  const schedule = (next: boolean, delay: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(next), delay);
  };
  const close = () => {
    clearTimeout(timer.current);
    setOpen(false);
  };
  useEffect(() => () => clearTimeout(timer.current), []);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (!anchor.current?.contains(event.target as Node)) {
        clearTimeout(timer.current);
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);
  return (
    <span
      ref={anchor}
      className="hover-popup-anchor"
      onClickCapture={() => clearTimeout(timer.current)}
      onPointerEnter={(event) => {
        if (event.pointerType === "mouse") schedule(true, HOVER_OPEN_DELAY_MS);
      }}
      onPointerUp={(event) => {
        if (event.pointerType !== "mouse") {
          clearTimeout(timer.current);
          setOpen(true);
        }
      }}
      onPointerLeave={(event) => {
        if (event.pointerType !== "mouse") return;
        if (!(
          keyboard.current && anchor.current?.contains(document.activeElement)
        ))
          schedule(false, HOVER_CLOSE_DELAY_MS);
      }}
      onFocus={() => {
        if (keyboard.current) {
          clearTimeout(timer.current);
          setOpen(true);
        }
      }}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null))
          schedule(false, HOVER_CLOSE_DELAY_MS);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          close();
          event.stopPropagation();
        }
      }}
    >
      {renderTrigger(open, show)}
      <div
        ref={panel}
        className="hover-popup-position"
        data-align="right"
        hidden={!open}
      >
        <div
          id={id}
          role={role}
          aria-labelledby={labelledBy}
          className={`hover-popup-surface ${className ?? ""}`}
        >
          {children}
        </div>
      </div>
    </span>
  );
}
