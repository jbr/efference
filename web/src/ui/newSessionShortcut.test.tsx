// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { TopBar } from "./TopBar.tsx";
import { SessionList } from "./SessionList.tsx";

vi.mock("./SessionSwitcher.tsx", () => ({ SessionSwitcher: () => null }));
vi.mock("../api/sessions.ts", () => ({
  openSessions: () => ({ close() {} }),
  openSessionCard: () => ({ close() {} }),
}));
vi.mock("../api/http.ts", async (original) => ({
  ...(await original<typeof import("../api/http.ts")>()),
  fetchArchivedSessions: async () => [],
}));
vi.mock("./NewSessionForm.tsx", () => ({
  NewSessionForm: ({ page }: { page: string }) => <div data-page={page} />,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({
    matches: true,
    addEventListener() {},
    removeEventListener() {},
  }));
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  history.replaceState(null, "", "/");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function press() {
  const event = new KeyboardEvent("keydown", {
    key: "n",
    metaKey: true,
    bubbles: true,
    cancelable: true,
  });
  act(() => document.body.dispatchEvent(event));
  return event;
}

test("session Cmd+N follows the project's + destination", () => {
  history.replaceState(null, "", "/sessions/alpha/");
  act(() =>
    root.render(
      <TopBar
        listHref="/"
        project="/work/a & b"
        identity={null}
        usage={null}
        upstreamFailure={null}
        upstreamRetry={null}
      />,
    ),
  );
  const destination = host.querySelector(".session-new")!.getAttribute("href");
  expect(press().defaultPrevented).toBe(true);
  expect(location.pathname + location.search).toBe(destination);
});

test("home Cmd+N opens location selection and leaves launch pages alone", async () => {
  await act(async () => root.render(<SessionList sessions={[]} />));
  expect(press().defaultPrevented).toBe(true);
  expect(location.search).toBe("?new=location");
  expect(host.querySelector('[data-page="location"]')).not.toBeNull();
  expect(press().defaultPrevented).toBe(false);
});
