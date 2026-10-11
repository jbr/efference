// @vitest-environment jsdom
// The lightbox host over the module store (lightbox.ts): opening an image
// renders it, and the two close paths the model has — Escape and a backdrop
// tap — behave, while a tap on the picture itself does not close.

import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { LightboxHost } from "./Lightbox.tsx";
import { closeLightbox, openLightbox } from "./lightbox.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  // jsdom has no pointer-capture; the swipe handlers call it on the stage.
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => {
  act(() => closeLightbox());
  vi.useRealTimers();
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

test("opening an image renders it, and Escape fades it out", () => {
  vi.useFakeTimers();
  const f = mount();
  expect(f.host.querySelector(".lightbox-image")).toBeNull();
  act(() => openLightbox({ src: "/v1/img", alt: "a picture" }));
  const image = f.host.querySelector<HTMLImageElement>(".lightbox-image");
  expect(image?.getAttribute("src")).toBe("/v1/img");
  expect(f.host.querySelector(".lightbox")?.getAttribute("aria-label")).toBe(
    "a picture",
  );
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });
  expect(f.host.querySelector(".lightbox")?.className).toContain("closing");
  act(() => vi.advanceTimersByTime(250));
  expect(f.host.querySelector(".lightbox")).toBeNull();
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
