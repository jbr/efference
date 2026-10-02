// Query-backed launch pages (docs/web-session-creation.md). The host already serves `/`;
// the query identifies a surface without introducing another server SPA path.
import { useCallback, useEffect, useState } from "react";
import type { Role } from "./pickerRows.ts";

export type LaunchPage = "location" | "settings" | Role;

export function launchPage(search: string): LaunchPage | null {
  const params = new URLSearchParams(search);
  const page = params.get("new");
  if (page === "location" || page === "settings") return page;
  const role = params.get("role");
  return page === "model" &&
    (role === "main" || role === "spotter" || role === "triage")
    ? role
    : null;
}

export function launchUrl(current: string, page: LaunchPage | null): string {
  const url = new URL(current);
  url.searchParams.delete("new");
  url.searchParams.delete("role");
  if (page === "location" || page === "settings") {
    url.searchParams.set("new", page);
  } else if (page !== null) {
    url.searchParams.set("new", "model");
    url.searchParams.set("role", page);
  }
  return `${url.pathname}${url.search}${url.hash}`;
}

export function useLaunchNavigation() {
  const [page, setPage] = useState(() =>
    typeof location === "undefined" ? null : launchPage(location.search),
  );
  useEffect(() => {
    const onPop = () => setPage(launchPage(location.search));
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const navigate = useCallback((next: LaunchPage) => {
    const from = launchPage(location.search);
    const ownedDepth = history.state?.launchDepth;
    const depth =
      from === null
        ? 1
        : typeof ownedDepth === "number"
          ? ownedDepth + 1
          : undefined;
    history.pushState(
      { launchDepth: depth, launchFrom: from },
      "",
      launchUrl(location.href, next),
    );
    setPage(next);
    window.scrollTo(0, 0);
  }, []);

  const backTo = useCallback((next: LaunchPage) => {
    if (history.state?.launchFrom === next) {
      history.back();
    } else {
      history.replaceState(history.state, "", launchUrl(location.href, next));
      setPage(next);
    }
  }, []);

  const close = useCallback(() => {
    const depth = history.state?.launchDepth;
    if (typeof depth === "number" && depth > 0) {
      history.go(-depth);
    } else {
      history.replaceState(null, "", launchUrl(location.href, null));
      setPage(null);
    }
  }, []);

  return { page, navigate, backTo, close };
}
