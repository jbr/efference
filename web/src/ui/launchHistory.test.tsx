// @vitest-environment jsdom
import { act, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useLaunchNavigation } from "./newSessionNavigation.ts";
import { installLinkNavigation, navigate } from "./route.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let host: HTMLDivElement;
let root: Root;
let launch: ReturnType<typeof useLaunchNavigation>;
let removeLinks: () => void;
function Probe() {
  const navigation = useLaunchNavigation();
  useLayoutEffect(() => {
    launch = navigation;
  });
  return <span>{navigation.page ?? "home"}</span>;
}
beforeEach(() => {
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  history.replaceState(null, "", "/");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  removeLinks = installLinkNavigation();
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  removeLinks();
  vi.restoreAllMocks();
});
async function pop(action: () => void) {
  await act(async () => {
    const popped = new Promise<void>((resolve) =>
      window.addEventListener("popstate", () => resolve(), { once: true }),
    );
    action();
    await popped;
  });
}

test.each(["/", "/sessions/alpha/?view=all"])(
  "cancelling unwinds launch stages to %s",
  async (origin) => {
    history.replaceState(null, "", origin);
    act(() => navigate("/?new=settings&cwd=%2Fwork"));
    act(() => root.render(<Probe />));
    expect(launch.page).toBe("settings");
    act(() => launch.navigate("main"));
    expect(history.state.launchDepth).toBe(2);
    await pop(() => launch.backTo("settings"));
    expect(launch.page).toBe("settings");
    await pop(() => launch.close());
    expect(location.pathname + location.search).toBe(origin);
    expect(launch.page).toBeNull();
  },
);

test("plain project links establish the same launch origin as keyboard navigation", async () => {
  history.replaceState(null, "", "/sessions/alpha/");
  act(() => root.render(<Probe />));
  const link = document.createElement("a");
  link.href = "/?new=settings&cwd=%2Fwork";
  host.append(link);
  act(() => link.click());
  expect(launch.page).toBe("settings");
  expect(history.state.launchDepth).toBe(1);
  await pop(() => launch.close());
  expect(location.pathname).toBe("/sessions/alpha/");
});

test("direct launch URLs never unwind unrelated history, even after more stages", () => {
  history.replaceState(null, "", "/?new=settings&cwd=%2Fwork");
  act(() => root.render(<Probe />));
  act(() => launch.navigate("main"));
  act(() => launch.navigate("triage"));
  act(() => launch.backTo("settings"));
  expect(launch.page).toBe("settings");
  const go = vi.spyOn(history, "go");
  act(() => launch.close());
  expect(go).not.toHaveBeenCalled();
  expect(location.pathname + location.search).toBe("/");
  expect(launch.page).toBeNull();
});

test("fallback stage replacement preserves owned depth; explicit home does not cancel to origin", async () => {
  history.replaceState(null, "", "/sessions/alpha/");
  act(() => navigate("/?new=location"));
  act(() => root.render(<Probe />));
  act(() => launch.backTo("settings"));
  expect(history.state.launchDepth).toBe(1);
  await pop(() => launch.close());
  expect(location.pathname).toBe("/sessions/alpha/");
  act(() => navigate("/?new=settings"));
  act(() => navigate("/"));
  expect(location.pathname + location.search).toBe("/");
  expect(launch.page).toBeNull();
  expect(history.state).toBeNull();
});
