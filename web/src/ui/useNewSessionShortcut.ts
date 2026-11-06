// Installed-app session creation (docs/web-session-creation.md).
import { useEffect } from "react";
import { standaloneApp } from "./tabShortcuts.ts";

export function useNewSessionShortcut(open: (() => void) | null) {
  useEffect(() => {
    if (open === null) return;
    const handler = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        !standaloneApp() ||
        !event.metaKey ||
        event.ctrlKey ||
        event.altKey ||
        event.shiftKey ||
        event.key.toLowerCase() !== "n"
      )
        return;
      event.preventDefault();
      if (!event.repeat) open();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open]);
}
