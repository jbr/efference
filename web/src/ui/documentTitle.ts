// Browser tab identity follows the active view (docs/web-client.md).
import { useLayoutEffect } from "react";
import { standaloneApp } from "./tabShortcuts.ts";

export const APP_TITLE = "efference";

/** The title for a session's view: the app title alone when no session is open,
 * the bare session name in the installed app (whose window frame already shows
 * the app name), and `<name> · efference` in an ordinary browser tab. */
export function sessionDocumentTitle(name: string | null): string {
  if (name === null) return APP_TITLE;
  return standaloneApp() ? name : `${name} · ${APP_TITLE}`;
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
