// Shared header frame and home wordmark (docs/web-client.md).
// Launch navigation can handle an ordinary home click without reloading its
// draft owner; modified clicks retain the link's native browser behavior.
import type { ReactNode, Ref } from "react";

export function AppHeader({
  children,
  homeHref = "/",
  onHome,
  className = "top-bar",
  innerClassName = "top-bar-inner",
  ref,
}: {
  children?: ReactNode;
  homeHref?: string | null;
  onHome?: () => void;
  className?: string;
  innerClassName?: string;
  ref?: Ref<HTMLElement>;
}) {
  return (
    <header className={className} ref={ref}>
      <div className={innerClassName}>
        {homeHref === null ? (
          <span className="wordmark">efference</span>
        ) : (
          <a
            className="wordmark"
            href={homeHref}
            title="All sessions"
            onClick={(event) => {
              if (
                onHome !== undefined &&
                !event.defaultPrevented &&
                event.button === 0 &&
                !event.metaKey &&
                !event.ctrlKey &&
                !event.shiftKey &&
                !event.altKey
              ) {
                event.preventDefault();
                onHome();
              }
            }}
          >
            efference
          </a>
        )}
        {children}
      </div>
    </header>
  );
}
