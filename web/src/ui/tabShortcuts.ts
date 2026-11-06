// Installed-app tab cycling (docs/web-client.md). Browser windows retain their
// native tab shortcuts; focus inside the app does not change the bindings.
export function tabDirection(
  event: Pick<
    KeyboardEvent,
    "code" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey"
  >,
  standalone: boolean,
): -1 | 1 | null {
  if (!standalone || event.altKey) return null;
  if (event.metaKey && event.shiftKey && !event.ctrlKey) {
    if (event.code === "BracketLeft") return -1;
    if (event.code === "BracketRight") return 1;
  }
  if (event.ctrlKey && !event.metaKey) {
    if (event.code === "Tab") return event.shiftKey ? -1 : 1;
    if (!event.shiftKey && event.code === "PageUp") return -1;
    if (!event.shiftKey && event.code === "PageDown") return 1;
  }
  return null;
}

export function standaloneApp(): boolean {
  return (
    window.matchMedia?.("(display-mode: standalone)").matches === true ||
    (navigator as Navigator & { standalone?: boolean }).standalone === true
  );
}

export function adjacentTab(
  tabs: readonly string[],
  current: string,
  direction: -1 | 1,
): string | null {
  const index = tabs.indexOf(current);
  if (tabs.length < 2 || index === -1) return null;
  return tabs[(index + direction + tabs.length) % tabs.length];
}
