// Client-side navigation between the host's pages (docs/web-client.md): the
// list at `/` and one session's view at `/sessions/<id>/`. The page is loaded
// once; entering a session, switching to another, or returning to the list is a
// `pushState` and a re-render, so the shell, the header and the other
// sessions' warm stores (session/registry.ts) survive the move.
//
// Only those two routes are client-side. Every other same-origin path (the
// attachment links, `/auth/…`) is left to the browser, and so is anything the
// browser would open elsewhere — a modified click, a `target`, a download.

import { useSyncExternalStore } from "react";
import { deriveRoute } from "../api/base.ts";

const listeners = new Set<() => void>();
const notify = () => listeners.forEach((listener) => listener());

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  window.addEventListener("popstate", listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("popstate", listener);
  };
}

/** The page's pathname, re-read on every navigation and every pop. */
export function usePathname(): string {
  return useSyncExternalStore(subscribe, () => location.pathname);
}

/** Whether the client renders the page at `pathname` itself: the list, or a
 * session's own view (not a route inside one — `/tree` is the session view's
 * to read). */
export function isClientPath(pathname: string): boolean {
  if (pathname === "/") return true;
  return pathname.startsWith("/sessions/") && deriveRoute(pathname) === "/";
}

const isClientRoute = (url: URL): boolean =>
  url.origin === location.origin && isClientPath(url.pathname);

/**
 * Go to `href` without loading the page when it is a client route, and by an
 * ordinary navigation when it is not. A same-URL move is not a history entry.
 */
export function navigate(href: string, mode: "push" | "replace" = "push") {
  const url = new URL(href, location.href);
  if (!isClientRoute(url)) {
    if (mode === "replace") location.replace(url);
    else location.assign(url);
    return;
  }
  if (url.pathname === location.pathname && url.search === location.search) {
    return;
  }
  const target = url.pathname + url.search;
  if (mode === "replace") history.replaceState(null, "", target);
  else history.pushState(null, "", target);
  window.scrollTo(0, 0);
  notify();
}

/** The `<a>` a click landed in, when the click is the plain primary-button
 * kind a browser would follow in this tab. */
function followedAnchor(event: MouseEvent): HTMLAnchorElement | null {
  if (event.defaultPrevented || event.button !== 0) return null;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
    return null;
  }
  const anchor = (event.target as Element | null)?.closest("a");
  if (!anchor || !anchor.hasAttribute("href")) return null;
  if (anchor.hasAttribute("download")) return null;
  const target = anchor.getAttribute("target");
  return target === null || target === "" || target === "_self" ? anchor : null;
}

/** Route plain clicks on links to client routes through `navigate`, so the
 * links stay real anchors (new tab, copy address, no script) and every one of
 * them — tab, card, menu row, header link — behaves the same. */
export function installLinkNavigation(): () => void {
  const onClick = (event: MouseEvent) => {
    const anchor = followedAnchor(event);
    if (anchor === null) return;
    const url = new URL(anchor.href, location.href);
    if (!isClientRoute(url)) return;
    event.preventDefault();
    navigate(url.pathname + url.search);
  };
  document.addEventListener("click", onClick);
  return () => document.removeEventListener("click", onClick);
}
