// @vitest-environment jsdom
// The lightbox host over the module store (lightbox.ts): opening an image
// renders it, and the two close paths the model has — Escape and a backdrop
// tap — behave, while a tap on the picture itself does not close.

import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { LightboxHost } from "./Lightbox.tsx";
import { closeLightbox, openLightbox } from "./lightbox.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  // jsdom has no pointer-capture; the swipe handlers call it on the stage.
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
});

// The viewer fetches the picture it is handed (useApiImage.ts). By default the
// request stays unanswered, so a test about gestures sees no late state update.
const createObjectURL = vi.fn<(bytes: Blob) => string>(() => "blob:lightbox");
const revokeObjectURL = vi.fn();
const fetchMock = vi.fn<typeof fetch>();
beforeEach(() => {
  createObjectURL.mockClear();
  revokeObjectURL.mockClear();
  fetchMock.mockReset();
  fetchMock.mockImplementation(() => new Promise<Response>(() => {}));
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL = createObjectURL;
      static revokeObjectURL = revokeObjectURL;
    },
  );
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  act(() => closeLightbox());
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function mount() {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => root.render(<LightboxHost />));
  return {
    host,
    close: () => act(() => root.unmount()),
  };
}

test("opening an image shows its fetched bytes, and Escape fades it out", async () => {
  vi.useFakeTimers();
  fetchMock.mockImplementation(() =>
    Promise.resolve(new Response(new Uint8Array([1, 2, 3]))),
  );
  const f = mount();
  expect(f.host.querySelector(".lightbox-image")).toBeNull();
  act(() => openLightbox({ src: "/v1/img", alt: "a picture" }));
  const image = f.host.querySelector<HTMLImageElement>(".lightbox-image");
  // The picture is on the page before its bytes are, with nothing to draw —
  // and no alt text standing in for it.
  expect(image?.hasAttribute("src")).toBe(false);
  expect(image?.getAttribute("alt")).toBe("");
  await act(() => vi.advanceTimersByTimeAsync(0));
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual(["/v1/img"]);
  expect(image?.getAttribute("src")).toBe("blob:lightbox");
  expect(image?.getAttribute("alt")).toBe("a picture");
  expect(f.host.querySelector(".lightbox")?.getAttribute("aria-label")).toBe(
    "a picture",
  );
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });
  expect(f.host.querySelector(".lightbox")?.className).toContain("closing");
  // The bytes stay up through the fade and go with the viewer.
  expect(revokeObjectURL).not.toHaveBeenCalled();
  await act(() => vi.advanceTimersByTimeAsync(250));
  expect(f.host.querySelector(".lightbox")).toBeNull();
  expect(revokeObjectURL.mock.calls).toEqual([["blob:lightbox"]]);
  f.close();
});

test("an image that cannot be fetched leaves the viewer open on an empty picture", async () => {
  vi.useFakeTimers();
  fetchMock.mockImplementation(() => Promise.reject(new TypeError("offline")));
  const f = mount();
  act(() => openLightbox({ src: "/v1/gone", alt: "missing" }));
  await act(() => vi.advanceTimersByTimeAsync(0));
  const image = f.host.querySelector<HTMLImageElement>(".lightbox-image");
  expect(image?.hasAttribute("src")).toBe(false);
  expect(image?.getAttribute("alt")).toBe("");
  // The name is still on the dialog and in the head bar.
  expect(f.host.querySelector(".lightbox")?.getAttribute("aria-label")).toBe(
    "missing",
  );
  expect(f.host.querySelector(".lightbox-caption")?.textContent).toBe(
    "missing",
  );
  expect(f.host.querySelector(".lightbox")?.className).not.toContain("closing");
  f.close();
});

test("a swipe at fit dismisses, while a drag too small springs back", () => {
  const f = mount();
  act(() => openLightbox({ src: "/v1/img", alt: "x" }));
  const stage = f.host.querySelector<HTMLElement>(".lightbox-stage")!;
  const drag = (to: [number, number]) =>
    act(() => {
      stage.dispatchEvent(
        new MouseEvent("pointerdown", {
          clientX: 100,
          clientY: 100,
          bubbles: true,
        }),
      );
      stage.dispatchEvent(
        new MouseEvent("pointermove", {
          clientX: to[0],
          clientY: to[1],
          bubbles: true,
        }),
      );
      stage.dispatchEvent(
        new MouseEvent("pointerup", {
          clientX: to[0],
          clientY: to[1],
          bubbles: true,
        }),
      );
    });
  drag([108, 104]); // ~9px — a tap's jitter
  expect(f.host.querySelector(".lightbox")?.className).not.toContain("closing");
  drag([100, 240]); // 140px — a deliberate swipe
  expect(f.host.querySelector(".lightbox")?.className).toContain("closing");
  f.close();
});

test("a retargeted click on the picture does not close", () => {
  // The stage holds pointer capture, so the browser retargets the click that
  // follows a tap on the picture to the stage itself. The handler must use the
  // recorded pointerdown target, not the click's, or every tap closes.
  const f = mount();
  act(() => openLightbox({ src: "/v1/img", alt: "x" }));
  const stage = f.host.querySelector<HTMLElement>(".lightbox-stage")!;
  const image = f.host.querySelector<HTMLElement>(".lightbox-image")!;
  act(() => {
    image.dispatchEvent(
      new MouseEvent("pointerdown", {
        clientX: 50,
        clientY: 50,
        bubbles: true,
      }),
    );
    image.dispatchEvent(
      new MouseEvent("pointerup", { clientX: 50, clientY: 50, bubbles: true }),
    );
    stage.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  expect(f.host.querySelector(".lightbox")?.className).not.toContain("closing");
  f.close();
});

test("a tap on the backdrop closes; a tap on the picture does not", () => {
  const f = mount();
  act(() => openLightbox({ src: "/v1/img", alt: "x" }));
  const stage = f.host.querySelector<HTMLElement>(".lightbox-stage");
  const image = f.host.querySelector<HTMLElement>(".lightbox-image");
  act(() => image?.click());
  expect(f.host.querySelector(".lightbox")?.className).not.toContain("closing");
  act(() => stage?.click());
  expect(f.host.querySelector(".lightbox")?.className).toContain("closing");
  f.close();
});
