import type { ReactNode } from "react";
import "./Disclosure.css";

/** Native disclosure interaction shared by secondary detail sections (docs/web-client.md). */
export function Disclosure({
  label,
  children,
  className = "",
}: {
  label: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <details className={`disclosure ${className}`}>
      <summary>{label}</summary>
      {children}
    </details>
  );
}
