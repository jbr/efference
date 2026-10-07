import { useCallback, useEffect, useState } from "react";

export type ResumeRoute =
  { stage: "location" } | { stage: "picker"; cwd: string };

export function resumePage(search: string): ResumeRoute | null {
  const params = new URLSearchParams(search);
  const stage = params.get("resume");
  if (stage === "location") return { stage: "location" };
  if (stage === "picker") {
    const cwd = params.get("cwd");
    if (cwd) return { stage: "picker", cwd };
  }
  return null;
}

export function resumeUrl(current: string, route: ResumeRoute | null): string {
  const url = new URL(current);
  url.searchParams.delete("new");
  url.searchParams.delete("role");
  url.searchParams.delete("resume");
  url.searchParams.delete("cwd");

  if (route?.stage === "location") {
    url.searchParams.set("resume", "location");
  } else if (route?.stage === "picker") {
    url.searchParams.set("resume", "picker");
    url.searchParams.set("cwd", route.cwd);
  }

  return `${url.pathname}${url.search}${url.hash}`;
}

export function useResumeNavigation() {
  const [route, setRoute] = useState<ResumeRoute | null>(() =>
    resumePage(typeof location === "undefined" ? "" : location.search),
  );

  useEffect(() => {
    const onPop = () => {
      setRoute(resumePage(location.search));
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const navigate = useCallback((next: ResumeRoute) => {
    const from = resumePage(location.search);
    const ownedDepth = history.state?.resumeDepth;
    const depth =
      from === null
        ? 1
        : typeof ownedDepth === "number"
          ? ownedDepth + 1
          : undefined;
    history.pushState(
      { resumeDepth: depth, resumeFrom: from },
      "",
      resumeUrl(location.href, next),
    );
    setRoute(next);
    window.scrollTo(0, 0);
  }, []);

  const close = useCallback(() => {
    const depth = history.state?.resumeDepth;
    if (typeof depth === "number" && depth > 0) {
      history.go(-depth);
    } else {
      history.replaceState(null, "", resumeUrl(location.href, null));
      setRoute(null);
    }
  }, []);

  return { route, navigate, close };
}
