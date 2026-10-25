// The `⋯` menu a card or row carries (docs/web-client.md): the trigger holds the
// open state, Escape or a pointer outside closes it, and the items are the
// caller's. `onClose` runs on every close — the caller's items may hold state of
// their own (a confirmation) that a closed menu should drop.

import { type ReactNode, useEffect, useRef, useState } from "react";

export function ActionsMenu({
  label,
  align = "down",
  className = "card-menu",
  onClose,
  children,
}: {
  /** The trigger's accessible name: what the menu acts on. */
  label: string;
  /** Which way the items open. A menu at the bottom of a clipped container (the
   * archived table) opens upward to stay whole. */
  align?: "down" | "up";
  className?: string;
  onClose?: () => void;
  /** The items, given the menu's own close so an item can be the last thing
   * that runs before it shuts. */
  children: (close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent | KeyboardEvent) => {
      if (
        event instanceof KeyboardEvent
          ? event.key === "Escape"
          : !ref.current?.contains(event.target as Node)
      ) {
        setOpen(false);
        onClose?.();
      }
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open, onClose]);

  const close = () => {
    setOpen(false);
    onClose?.();
  };

  return (
    <div className={className} ref={ref}>
      <button
        className="card-menu-button"
        aria-label={label}
        aria-expanded={open}
        onClick={() => (open ? close() : setOpen(true))}
      >
        ⋯
      </button>
      {open && (
        <div
          className={
            align === "up"
              ? "card-menu-list card-menu-list-up"
              : "card-menu-list"
          }
          role="menu"
        >
          {children(close)}
        </div>
      )}
    </div>
  );
}
