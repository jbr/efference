// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ListedSession } from "../api/types.ts";
import { SessionSwitcher, Tab } from "./SessionSwitcher.tsx";
import type { CardModel } from "./overview.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let listing: ListedSession[];
let updateListing: (sessions: ListedSession[]) => void;
vi.mock("../session/listing.ts", () => ({
  lastListing: () => listing,
  rememberListing: vi.fn(),
}));
vi.mock("../session/registry.ts", () => ({ prefetchStore: vi.fn() }));
vi.mock("../api/sessions.ts", () => ({
  openSessions: (update: typeof updateListing) => {
    updateListing = update;
    return { close: vi.fn() };
  },
  openSessionCard: () => ({ close: vi.fn() }),
}));

let host: HTMLDivElement;
let root: Root;
let room: number;
const fetchMock = vi.fn<typeof fetch>();
beforeEach(() => {
  listing = [
    { id: "alpha", title: "Alpha's work", status: "idle", processes: 0 },
    { id: "beta", title: "Beta's work", status: "idle", processes: 0 },
  ];
  room = 800;
  history.replaceState(null, "", "/sessions/alpha/");
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(
    () => room,
  );
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({
    left: 0,
    right: 100,
    width: 100,
    top: 0,
    bottom: 20,
    height: 20,
    x: 0,
    y: 0,
    toJSON() {},
  });
  vi.spyOn(globalThis, "getComputedStyle").mockReturnValue({
    paddingLeft: "0px",
    paddingRight: "0px",
    columnGap: "0px",
  } as CSSStyleDeclaration);
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
const mount = (currentId = "alpha") =>
  act(() => root.render(<SessionSwitcher currentId={currentId} />));
const tab = (id = "alpha") =>
  host.querySelector<HTMLAnchorElement>(
    `.switcher-tab[href="/sessions/${id}/"]`,
  )!;
const action = (label: string) =>
  [...host.querySelectorAll<HTMLButtonElement>(".archive-action button")].find(
    (button) => button.textContent === label,
  )!;
const click = (node: HTMLElement) => act(() => node.click());
function pointer(node: Element, type: string, pointerType = "mouse") {
  const event = new Event(type, { bubbles: true });
  Object.defineProperty(event, "pointerType", { value: pointerType });
  act(() => node.dispatchEvent(event));
}
const advance = (ms: number) => act(() => vi.advanceTimersByTime(ms));
const response = () =>
  new Response(JSON.stringify({ id: "alpha", state: "archived" }), {
    status: 200,
  });
function pendingRequest() {
  let resolve!: (response: Response) => void;
  fetchMock.mockReturnValue(
    new Promise<Response>((done) => {
      resolve = done;
    }),
  );
  return async () => {
    await act(async () => resolve(response()));
  };
}

test("tabs show session IDs rather than titles, including the selected tab", () => {
  const card = {
    id: "crisp-delta",
    title: "A long changing title",
  } as CardModel;
  for (const current of [false, true]) {
    const markup = renderToStaticMarkup(
      <Tab card={card} inert current={current} />,
    );
    expect(markup).toContain('class="switcher-label">crisp-delta</span>');
    expect(markup).not.toContain(card.title);
    expect(markup.includes("is-current")).toBe(current);
  }
});

test("hover bridges into the separate archive footer and confirmation stays open", () => {
  vi.useFakeTimers();
  listing[0] = { ...listing[0], status: "running", processes: 2 };
  mount();
  pointer(tab(), "pointerover");
  advance(179);
  expect(host.querySelector('[role="dialog"]')).toBeNull();
  advance(1);
  const archive = action("Archive session");
  expect(archive.closest("a")).toBeNull();
  pointer(tab(), "pointerout");
  advance(60);
  pointer(archive, "pointerover");
  advance(180);
  pointer(archive, "pointerdown");
  act(() => archive.focus());
  click(archive);
  pointer(archive, "pointerout");
  advance(500);
  expect(host.textContent).toContain(
    "is mid-turn and holds 2 background processes",
  );
  expect(action("Archive anyway")).toBeDefined();
  expect(fetchMock).not.toHaveBeenCalled();
  click(action("Cancel"));
  expect(action("Archive session")).toBeDefined();
  expect(fetchMock).not.toHaveBeenCalled();
  act(() =>
    action("Archive session").dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  advance(500);
  expect(host.querySelector('[role="dialog"]')).toBeNull();
  expect(document.activeElement).toBe(tab());
});

test("keyboard focus crosses into actions, while selected-tab taps pin details until dismissal", () => {
  vi.useFakeTimers();
  mount();
  act(() => tab().focus());
  expect(tab().getAttribute("aria-expanded")).toBe("true");
  act(() => action("Archive session").focus());
  advance(120);
  expect(host.querySelector('[role="dialog"]')).not.toBeNull();
  pointer(document.body, "pointerdown");
  expect(host.querySelector('[role="dialog"]')).toBeNull();
  pointer(tab(), "pointerdown", "touch");
  act(() => tab().focus());
  click(tab());
  pointer(tab(), "pointerout", "touch");
  advance(500);
  expect(host.querySelector('[role="dialog"]')).not.toBeNull();
  click(tab());
  expect(host.querySelector('[role="dialog"]')).toBeNull();
});

test("an idle session archives once and replaces the current route only after success", async () => {
  const finish = pendingRequest();
  mount();
  click(tab());
  const replace = vi.spyOn(history, "replaceState");
  const push = vi.spyOn(history, "pushState");
  click(action("Archive session"));
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(fetchMock).toHaveBeenCalledWith(
    "/sessions/alpha",
    expect.objectContaining({
      method: "DELETE",
      body: JSON.stringify({ source: "web" }),
    }),
  );
  expect(action("Archiving…").disabled).toBe(true);
  click(action("Archiving…"));
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(location.pathname).toBe("/sessions/alpha/");
  await finish();
  expect(location.pathname).toBe("/");
  expect(replace).toHaveBeenCalledTimes(1);
  expect(push).not.toHaveBeenCalled();
});

test.each(["running", "gated", "asking", "stalled", "parked"] as const)(
  "%s sessions require an explicit confirmation",
  async (status) => {
    listing[0] = { ...listing[0], status };
    fetchMock.mockResolvedValue(response());
    mount();
    click(tab());
    click(action("Archive session"));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(host.querySelector(".archive-warning")).not.toBeNull();
    await act(async () => action("Archive anyway").click());
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(location.pathname).toBe("/");
  },
);

test("background processes and live status changes use the latest warning", () => {
  mount();
  click(tab());
  act(() => updateListing([{ ...listing[0], processes: 1 }, listing[1]]));
  click(action("Archive session"));
  expect(host.textContent).toContain("holds 1 background process");
  expect(fetchMock).not.toHaveBeenCalled();
  click(action("Cancel"));
  act(() => updateListing([{ ...listing[0], status: "gated" }, listing[1]]));
  click(action("Archive session"));
  expect(host.textContent).toContain("is holding a call at a gate");
  expect(fetchMock).not.toHaveBeenCalled();
});

test("failure keeps the session accessible, shows the error, and allows retry", async () => {
  fetchMock.mockRejectedValueOnce(new Error("Host unavailable"));
  mount();
  click(tab());
  await act(async () => action("Archive session").click());
  expect(location.pathname).toBe("/sessions/alpha/");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "Host unavailable",
  );
  expect(action("Archive session").disabled).toBe(false);
  const finish = pendingRequest();
  click(action("Archive session"));
  expect(host.querySelector('[role="alert"]')).toBeNull();
  await finish();
  expect(location.pathname).toBe("/");
});

test("archiving another session leaves the selected session alone", async () => {
  vi.useFakeTimers();
  const finish = pendingRequest();
  mount();
  pointer(tab("beta"), "pointerover");
  advance(180);
  click(action("Archive session"));
  expect(fetchMock).toHaveBeenCalledWith(
    "/sessions/beta",
    expect.objectContaining({ method: "DELETE" }),
  );
  await finish();
  expect(location.pathname).toBe("/sessions/alpha/");
});

test("a completed archive does not redirect away from a view entered while it was pending", async () => {
  const finish = pendingRequest();
  mount();
  click(tab());
  click(action("Archive session"));
  history.pushState(null, "", "/sessions/beta/");
  await finish();
  expect(location.pathname).toBe("/sessions/beta/");
});

test("the selected tab stays visible when other tabs overflow, including a single live session", () => {
  room = 220;
  listing.push({ id: "gamma", status: "idle", processes: 0 });
  mount("gamma");
  expect(tab("gamma")).not.toBeNull();
  expect(host.querySelectorAll("nav > .switcher-slot")).toHaveLength(1);
  expect(host.querySelector(".switcher-more")).not.toBeNull();
  act(() => updateListing([listing[2]]));
  expect(tab("gamma")).not.toBeNull();
  click(tab("gamma"));
  expect(action("Archive session")).toBeDefined();
});

test("reopening a popup cannot duplicate its pending archive", async () => {
  const finish = pendingRequest();
  mount();
  click(tab());
  click(action("Archive session"));
  pointer(document.body, "pointerdown");
  click(tab());
  click(action("Archive session"));
  expect(fetchMock).toHaveBeenCalledTimes(1);
  await finish();
});

test("hover opens again after an archived pinned card leaves the listing", async () => {
  vi.useFakeTimers();
  fetchMock.mockResolvedValue(response());
  mount();
  pointer(tab("beta"), "pointerover");
  advance(180);
  await act(async () => action("Archive session").click());
  act(() => updateListing([listing[0]]));
  expect(host.querySelector('[role="dialog"]')).toBeNull();
  pointer(tab(), "pointerover");
  advance(180);
  expect(
    host.querySelector('[role="dialog"]')?.getAttribute("aria-label"),
  ).toBe("alpha session details");
});

test("modified current-tab clicks retain native link behavior", () => {
  mount();
  const event = new MouseEvent("click", {
    bubbles: true,
    cancelable: true,
    ctrlKey: true,
  });
  act(() => tab().dispatchEvent(event));
  expect(event.defaultPrevented).toBe(false);
  expect(host.querySelector('[role="dialog"]')).toBeNull();
});
