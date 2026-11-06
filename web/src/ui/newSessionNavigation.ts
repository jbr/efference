// Query-backed launch navigation (docs/web-session-creation.md).
import { useCallback } from "react";
import { navigate as routeNavigate, useSearch } from "./route.ts";
import { launchPage, launchUrl, type LaunchPage } from "./newSessionRoutes.ts";
export { launchPage, launchUrl, type LaunchPage } from "./newSessionRoutes.ts";

export function useLaunchNavigation() {
  const page = launchPage(useSearch());

  const navigate = useCallback((next: LaunchPage) => {
    routeNavigate(launchUrl(location.href, next));
    window.scrollTo(0, 0);
  }, []);

  const backTo = useCallback((next: LaunchPage) => {
    if (history.state?.launchFrom === next) {
      history.back();
    } else {
      routeNavigate(launchUrl(location.href, next), "replace");
    }
  }, []);

  const close = useCallback(() => {
    const depth = history.state?.launchDepth;
    if (typeof depth === "number" && depth > 0) {
      history.go(-depth);
    } else {
      routeNavigate(launchUrl(location.href, null), "replace");
    }
  }, []);

  return { page, navigate, backTo, close };
}
