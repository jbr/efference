import { useSyncExternalStore } from "react";
const query = () =>
  typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(max-width: 1099px)")
    : null;
function subscribe(notify: () => void) {
  const media = query();
  media?.addEventListener("change", notify);
  return () => media?.removeEventListener("change", notify);
}
export function useSidebarOverlay(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => query()?.matches ?? false,
    () => false,
  );
}
