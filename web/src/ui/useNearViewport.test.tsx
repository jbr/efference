// @vitest-environment jsdom
// The viewport gate (useNearViewport.ts): false until the observer reports the
// element intersecting, true from then on, and true from the start where there
// is no observer to ask.

import { act, useRef } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useNearViewport } from "./useNearViewport.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

function Probe({ margin }: { margin?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const near = useNearViewport(ref, margin);
  return <div ref={ref} data-near={String(near)} />;
}

/** The browser's observer, driven by hand. */
class FakeObserver {
  static all: FakeObserver[] = [];
  readonly targets = new Set<Element>();
  disconnected = false;
  constructor(
    private readonly callback: IntersectionObserverCallback,
    readonly options?: IntersectionObserverInit,
  ) {
    FakeObserver.all.push(this);
  }
  observe(target: Element) {
    this.targets.add(target);
  }
  unobserve(target: Element) {
    this.targets.delete(target);
  }
  disconnect() {
    this.targets.clear();
    this.disconnected = true;
  }
  takeRecords() {
    return [];
  }
  report(isIntersecting: boolean) {
    const entries = [...this.targets].map(
      (target) => ({ target, isIntersecting }) as IntersectionObserverEntry,
    );
    this.callback(entries, this as unknown as IntersectionObserver);
  }
}

beforeEach(() => {
  FakeObserver.all = [];
});
afterEach(() => vi.unstubAllGlobals());

function mount(node: React.ReactNode) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() => root.render(node));
  return {
    near: () => host.firstElementChild?.getAttribute("data-near"),
    unmount: () => act(() => root.unmount()),
  };
}

describe("without an IntersectionObserver", () => {
  it("is near from the first render", () => {
    const view = mount(<Probe />);
    expect(view.near()).toBe("true");
    view.unmount();
  });
});

describe("with an IntersectionObserver", () => {
  beforeEach(() => vi.stubGlobal("IntersectionObserver", FakeObserver));

  it("is not near until the observer reports the element intersecting", () => {
    const view = mount(<Probe />);
    expect(view.near()).toBe("false");
    expect(FakeObserver.all).toHaveLength(1);
    act(() => FakeObserver.all[0].report(false));
    expect(view.near()).toBe("false");
    act(() => FakeObserver.all[0].report(true));
    expect(view.near()).toBe("true");
    view.unmount();
  });

  it("watches with a margin past the top and bottom, or the one it is given", () => {
    const defaulted = mount(<Probe />);
    const given = mount(<Probe margin="40px" />);
    expect(FakeObserver.all.map((o) => o.options?.rootMargin)).toEqual([
      "1500px 0px",
      "40px",
    ]);
    defaulted.unmount();
    given.unmount();
  });

  it("stays near once it has been, and stops watching", () => {
    const view = mount(<Probe />);
    act(() => FakeObserver.all[0].report(true));
    expect(FakeObserver.all[0].disconnected).toBe(true);
    // Nothing is left to report it leaving, and a second observer is not made.
    expect(FakeObserver.all).toHaveLength(1);
    expect(view.near()).toBe("true");
    view.unmount();
  });

  it("lets go of the observer when the component leaves before arriving", () => {
    const view = mount(<Probe />);
    expect(FakeObserver.all[0].disconnected).toBe(false);
    view.unmount();
    expect(FakeObserver.all[0].disconnected).toBe(true);
  });
});
