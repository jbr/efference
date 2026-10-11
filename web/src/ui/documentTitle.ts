// Browser tab identity follows the active view (docs/web-client.md).
import { useLayoutEffect } from "react";

export const APP_TITLE = "efference";

export function sessionDocumentTitle(name: string | null): string {
  return name === null ? APP_TITLE : `${name} · ${APP_TITLE}`;
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
