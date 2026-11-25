// Browser tab identity follows the active view (docs/web-client.md).
import { useLayoutEffect } from "react";
import { standaloneApp } from "./tabShortcuts.ts";

export const APP_TITLE = "efference";

/** Use the app name first in standalone mode: the installed Chrome window
 * tested on macOS showed the session name only with that prefix. */
export function sessionDocumentTitle(name: string | null): string {
  if (name === null) return APP_TITLE;
  return standaloneApp() ? `${APP_TITLE} - ${name}` : `${name} - ${APP_TITLE}`;
}

/** null leaves ownership to the session view; other views restore the app title. */
export function useDocumentTitle(title: string | null): void {
  useLayoutEffect(() => {
    if (title === null) return;
    document.title = title;
    return () => {
      document.title = APP_TITLE;
    };
  }, [title]);
}
