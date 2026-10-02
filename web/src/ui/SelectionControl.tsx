// Selection fields in the launch form (docs/web-session-creation.md).
import type { ReactNode } from "react";
import "./SelectionControl.css";

export function SelectionControl({
  label,
  children,
  onClick,
}: {
  label: string;
  children: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="setting-pick"
      aria-label={label}
      onClick={onClick}
    >
      <span>{children}</span>
      <span aria-hidden="true">›</span>
    </button>
  );
}
