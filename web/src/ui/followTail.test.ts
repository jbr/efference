// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  departFromFrame,
  installFollowTail,
  pinTail,
  resumeTail,
  subscribeTail,
  tailFollowing,
} from "./followTail.ts";

let y = 0,
  height = 2000;
let resize: ResizeObserverCallback;
let frames: FrameRequestCallback[] = [];
let close: (() => void) | null = null;
const scroll = (next: number) => {
  y = next;
  window.dispatchEvent(new Event("scroll"));
};
const flush = () => {
  const next = frames;
  frames = [];
  for (const frame of next) frame(0);
};
const wheel = (deltaY = -100, target: EventTarget = window) =>
  target.dispatchEvent(new WheelEvent("wheel", { deltaY, bubbles: true }));
beforeEach(() => {
  y = 0;
  height = 2000;
  frames = [];
  document.body.replaceChildren();
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value: 800,
  });
  Object.defineProperty(window, "scrollY", {
    configurable: true,
    get: () => y,
  });
  Object.defineProperty(document.documentElement, "scrollHeight", {
    configurable: true,
    get: () => height,
  });
  Object.defineProperty(document.documentElement, "clientWidth", {
    configurable: true,
    value: 390,
  });
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn((frame: FrameRequestCallback) => {
      frames.push(frame);
      return frames.length;
    }),
  );
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(callback: ResizeObserverCallback) {
        resize = callback;
      }
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(window, "scrollTo").mockImplementation((options) => {
    const next = Math.max(
      0,
      Math.min(
        (options as ScrollToOptions).top ?? 0,
        height - window.innerHeight,
      ),
    );
    if (next !== y) scroll(next);
  });
  close = installFollowTail();
  flush();
});
afterEach(() => {
  close?.();
  close = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test("initial mount follows the bottom", () => {
  expect(y).toBe(1200);
  expect(tailFollowing()).toBe(true);
});
test("growth and delayed non-user scroll events do not discard intent", () => {
  height = 5000;
  window.dispatchEvent(new Event("scroll"));
  expect(tailFollowing()).toBe(true);
  pinTail();
  expect(y).toBe(4200);
  expect(tailFollowing()).toBe(true);
});
test("asynchronous layout growth and viewport resize pin without a React render", () => {
  height = 2500;
  resize([], {} as ResizeObserver);
  flush();
  expect(y).toBe(1700);
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value: 600,
  });
  window.dispatchEvent(new Event("resize"));
  flush();
  expect(y).toBe(1900);
  expect(tailFollowing()).toBe(true);
});
test("deliberate upward wheel departure preserves the reading position through growth", () => {
  wheel();
  scroll(1100);
  expect(tailFollowing()).toBe(false);
  height = 3000;
  resize([], {} as ResizeObserver);
  window.dispatchEvent(new Event("resize"));
  flush();
  pinTail();
  expect(y).toBe(1100);
});
test("actual bottom contact by any means re-engages, not merely getting close", () => {
  wheel();
  scroll(1100);
  scroll(1190);
  expect(tailFollowing()).toBe(false);
  scroll(1200);
  expect(tailFollowing()).toBe(true);
  height = 3000;
  pinTail();
  expect(y).toBe(2200);
});
test("latest explicitly resumes even before the scroll event arrives", () => {
  wheel();
  scroll(1000);
  resumeTail();
  expect(tailFollowing()).toBe(true);
  expect(y).toBe(1200);
});
test("keyboard departure disengages but editing and prevented inputs do not", () => {
  const editor = document.createElement("textarea");
  document.body.append(editor);
  editor.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Home", bubbles: true }),
  );
  expect(tailFollowing()).toBe(true);
  const prevented = new WheelEvent("wheel", { deltaY: -10, cancelable: true });
  prevented.preventDefault();
  window.dispatchEvent(prevented);
  window.dispatchEvent(new WheelEvent("wheel", { deltaY: -10, ctrlKey: true }));
  expect(tailFollowing()).toBe(true);
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "PageUp" }));
  scroll(400);
  pinTail();
  expect(tailFollowing()).toBe(false);
  expect(y).toBe(400);
});
test("nested upward scrolling does not take the window out of follow mode", () => {
  const panel = document.createElement("div");
  panel.style.overflowY = "auto";
  document.body.append(panel);
  Object.defineProperties(panel, {
    scrollHeight: { value: 300 },
    clientHeight: { value: 100 },
    scrollTop: { value: 50 },
  });
  wheel(-20, panel);
  expect(tailFollowing()).toBe(true);
});
test("touch scrolling away disengages and bottom arrival resumes", () => {
  const touch = (type: string, clientY: number) => {
    const event = new Event(type);
    Object.defineProperty(event, "touches", { value: [{ clientY }] });
    window.dispatchEvent(event);
  };
  touch("touchstart", 200);
  touch("touchmove", 240);
  scroll(1100);
  expect(tailFollowing()).toBe(false);
  touch("touchmove", 100);
  scroll(1200);
  expect(tailFollowing()).toBe(true);
});
test("dragging the scrollbar upward disengages", () => {
  const event = new MouseEvent("pointerdown", { clientX: 389 });
  Object.defineProperty(event, "pointerType", { value: "mouse" });
  window.dispatchEvent(event);
  scroll(400);
  expect(tailFollowing()).toBe(false);
  window.dispatchEvent(new Event("pointerup"));
  scroll(1200);
  expect(tailFollowing()).toBe(true);
});
test("subscribers share intent transitions and teardown removes input ownership", () => {
  const notify = vi.fn();
  const unsubscribe = subscribeTail(notify);
  wheel();
  scroll(1100);
  expect(notify).toHaveBeenCalledTimes(1);
  resumeTail();
  expect(notify).toHaveBeenCalledTimes(2);
  unsubscribe();
  close?.();
  close = null;
  wheel();
  expect(tailFollowing()).toBe(true);
  const old = y;
  height = 3000;
  pinTail();
  expect(y).toBe(old);
});

test("an upward input that does not move the viewport does not disengage", () => {
  wheel();
  expect(tailFollowing()).toBe(true);
  flush();
  expect(tailFollowing()).toBe(true);
  height = 3000;
  pinTail();
  expect(y).toBe(2200);
});

test("render growth cannot pin over an in-progress reader departure", () => {
  wheel();
  height = 3000;
  pinTail();
  expect(y).toBe(1200);
  scroll(1100);
  flush();
  expect(tailFollowing()).toBe(false);
  expect(y).toBe(1100);
});

test("a small deliberate departure is not swallowed by bottom-distance slack", () => {
  wheel(-5);
  scroll(1195);
  flush();
  expect(tailFollowing()).toBe(false);
  expect(y).toBe(1195);
});

test("bottom-contact renders, resize and scroll notifications do not issue redundant scroll commands", () => {
  vi.mocked(window.scrollTo).mockClear();
  pinTail();
  resize([], {} as ResizeObserver);
  window.dispatchEvent(new Event("scroll"));
  flush();
  expect(window.scrollTo).not.toHaveBeenCalled();
});

test("growth during a touch press waits until release and click dispatch before pinning", () => {
  const button = document.createElement("button");
  document.body.append(button);
  const clicked = vi.fn();
  button.addEventListener("click", clicked);
  const touch = new Event("touchstart", { bubbles: true });
  Object.defineProperty(touch, "touches", { value: [{ clientY: 300 }] });
  button.dispatchEvent(touch);
  height = 2600;
  resize([], {} as ResizeObserver);
  flush();
  pinTail();
  expect(y).toBe(1200);
  expect(tailFollowing()).toBe(true);
  button.dispatchEvent(new Event("touchend", { bubbles: true }));
  button.click();
  expect(clicked).toHaveBeenCalledTimes(1);
  expect(y).toBe(1200);
  flush();
  expect(y).toBe(1800);
});

test("mouse presses also defer layout-driven pinning until release", () => {
  window.dispatchEvent(new MouseEvent("pointerdown", { clientX: 180 }));
  height = 2600;
  pinTail();
  expect(y).toBe(1200);
  window.dispatchEvent(new Event("pointerup"));
  flush();
  expect(y).toBe(1800);
});

// A wheel or touch over an artifact goes to the frame's document; the window
// sees only the scroll the browser chains to it once the frame can scroll no
// further. Alone that reads as layout and is pinned back, so the frame reports
// the departure itself.
test("a scroll chained from a frame is pinned back unless the frame reports leaving", () => {
  scroll(1100);
  flush();
  expect(y).toBe(1200);
  expect(tailFollowing()).toBe(true);

  departFromFrame();
  scroll(1100);
  flush();
  expect(tailFollowing()).toBe(false);
  expect(y).toBe(1100);
});
