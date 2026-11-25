// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installLinkNavigation, isClientPath } from "./route.ts";

describe("which paths the client navigates itself", () => {
  it("takes the list and a session's own view", () => {
    expect(isClientPath("/")).toBe(true);
    expect(isClientPath("/sessions/a-b/")).toBe(true);
    expect(isClientPath("/sessions/a-b")).toBe(true);
  });

  it("leaves routes inside a session and the listener's own paths to the browser", () => {
    expect(isClientPath("/sessions/a-b/tree")).toBe(false);
    expect(isClientPath("/sessions/a-b/v1/state")).toBe(false);
    expect(isClientPath("/pair")).toBe(false);
    expect(isClientPath("/auth/link")).toBe(false);
    expect(isClientPath("/sessions/")).toBe(false);
    expect(isClientPath("/sessions/../x/")).toBe(false);
  });
});

describe("plain link clicks", () => {
  let uninstall: () => void;

  beforeEach(() => {
    window.history.replaceState(null, "", "/?gallery");
    vi.spyOn(window, "scrollTo").mockImplementation(() => {});
    uninstall = installLinkNavigation();
  });

  afterEach(() => {
    uninstall();
    vi.restoreAllMocks();
    document.body.replaceChildren();
  });

  /** Dispatch a plain primary-button click on a fresh anchor, as a person
   * clicking a link does, and report whether the router took it. */
  function click(href: string): MouseEvent {
    const anchor = document.createElement("a");
    anchor.href = href;
    anchor.textContent = "link";
    document.body.append(anchor);
    const event = new MouseEvent("click", {
      bubbles: true,
      cancelable: true,
      button: 0,
    });
    anchor.dispatchEvent(event);
    return event;
  }

  it("leaves a same-page fragment link to the browser", () => {
    // The gallery index links `#session-tabs`: the browser's own same-document
    // navigation scrolls to the section and records the fragment. jsdom does
    // neither, so the test pins only that the router declined to intercept it.
    const event = click("#session-tabs");
    expect(event.defaultPrevented).toBe(false);
    expect(location.pathname).toBe("/");
    expect(location.search).toBe("?gallery");
  });

  it("still intercepts a same-page link with no fragment so the page does not reload", () => {
    const event = click("/?gallery");
    expect(event.defaultPrevented).toBe(true);
  });

  it("routes an ordinary link to another client page", () => {
    const event = click("/sessions/a-b/");
    expect(event.defaultPrevented).toBe(true);
    expect(location.pathname).toBe("/sessions/a-b/");
  });

  it("leaves a non-client link alone", () => {
    const event = click("/auth/link");
    expect(event.defaultPrevented).toBe(false);
  });
});
