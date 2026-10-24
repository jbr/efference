// @vitest-environment jsdom
// The components that draw API images — a sent attachment's thumbnail, a
// staged chip's, a result's picture — each resolve their URL through the shared
// cache (useApiImage.ts), and hand the lightbox the URL rather than what they
// resolved, so the viewer's hold on the bytes is its own. A result's picture
// also waits for its box to come near the viewport before it asks.

import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { Attachment } from "../api/types.ts";
import { LightboxHost } from "./Lightbox.tsx";
import { closeLightbox } from "./lightbox.ts";
import { SentAttachments, StagedAttachments } from "./Attachments.tsx";
import type { Staged } from "./staging.ts";
import { ResultImages } from "./ToolBlocks.tsx";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  // jsdom has no pointer-capture; the lightbox's stage calls it.
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
});

/** The browser's observer, driven by hand: the test says when an element comes
 * near. */
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
  /** Report every watched element as intersecting, or not. */
  report(isIntersecting: boolean) {
    const entries = [...this.targets].map(
      (target) => ({ target, isIntersecting }) as IntersectionObserverEntry,
    );
    this.callback(entries, this as unknown as IntersectionObserver);
  }
}

let made = 0;
const createObjectURL = vi.fn<(bytes: Blob) => string>(
  () => `blob:image-${++made}`,
);
const revokeObjectURL = vi.fn();
const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
  vi.useFakeTimers();
  made = 0;
  FakeObserver.all = [];
  createObjectURL.mockClear();
  revokeObjectURL.mockClear();
  fetchMock.mockReset();
  fetchMock.mockImplementation(() =>
    Promise.resolve(new Response(new Uint8Array([1, 2, 3]))),
  );
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL = createObjectURL;
      static revokeObjectURL = revokeObjectURL;
    },
  );
  vi.stubGlobal("fetch", fetchMock);
});
// Every view is unmounted when its test ends, pass or fail: the cache is
// module-level, so a holder left behind would answer for the next test's URL.
const mounted: (() => void)[] = [];
afterEach(() => {
  act(() => closeLightbox());
  for (const unmount of mounted.splice(0)) unmount();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function mount() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  mounted.push(() => act(() => root.unmount()));
  return {
    host,
    /** Render, then let the requests and the state updates they cause land. */
    render: (node: ReactNode) =>
      act(async () => {
        root.render(node);
        await vi.advanceTimersByTimeAsync(0);
      }),
    unmount: () => act(() => root.unmount()),
    thumbnails: () =>
      [...host.querySelectorAll("img:not(.lightbox-image)")].map((img) =>
        img.getAttribute("src"),
      ),
    lightboxSrc: () =>
      document.querySelector(".lightbox-image")?.getAttribute("src") ?? null,
  };
}

const picture: Attachment = {
  name: "pic.png",
  path: "/stored/pic.png",
  kind: { Image: { format: "PNG", width: 4, height: 3, bytes: 3 } },
};

test("a sent image's thumbnail is drawn from the fetched bytes", async () => {
  const view = mount();
  await view.render(<SentAttachments attachments={[picture]} />);
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/attachments/pic.png",
  ]);
  expect(view.thumbnails()).toEqual(["blob:image-1"]);
  await view.unmount();
});

test("the lightbox holds a sent image's bytes itself, so the thumbnail can leave first", async () => {
  // One tree shape throughout, so the viewer is not remounted when the
  // thumbnails go.
  const page = (thumbnails: boolean) => (
    <>
      {thumbnails && <SentAttachments attachments={[picture]} />}
      <LightboxHost />
    </>
  );
  const view = mount();
  await view.render(page(true));
  const open = view.host.querySelector<HTMLButtonElement>(
    "button.attachment-image",
  )!;
  await act(async () => {
    open.click();
    await vi.advanceTimersByTimeAsync(0);
  });
  // The viewer is handed the URL, and finds the thumbnail's request already
  // made: one fetch, one object URL, shown in both places.
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(view.lightboxSrc()).toBe("blob:image-1");

  // The thumbnail leaves the page; what the viewer shows is not revoked.
  await view.render(page(false));
  expect(view.thumbnails()).toEqual([]);
  expect(revokeObjectURL).not.toHaveBeenCalled();
  expect(view.lightboxSrc()).toBe("blob:image-1");

  // Closing the viewer — its fade included — is the last release.
  act(() => closeLightbox());
  await act(() => vi.advanceTimersByTimeAsync(250));
  expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
  await view.unmount();
});

test("a staged chip draws its local preview as given and its stored bytes by fetch", async () => {
  const chip = (key: string, preview: string | null): Staged => ({
    key,
    label: key,
    preview,
    status: "ready",
    attachment: picture,
  });
  const view = mount();
  await view.render(
    <StagedAttachments
      staged={[
        chip("local", "blob:local-preview"),
        chip("stored", "/v1/attachments/pic.png"),
        chip("note", null),
      ]}
      onRemove={() => {}}
      onInline={() => {}}
    />,
  );
  expect(view.thumbnails()).toEqual(["blob:local-preview", "blob:image-1"]);
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/attachments/pic.png",
  ]);
  await view.unmount();
  // The local preview belongs to the draft store, which revokes it.
  expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
});

test("a staged chip keeps drawing its local preview until the stored bytes arrive", async () => {
  const row = (preview: string) => (
    <StagedAttachments
      staged={[
        {
          key: "k",
          label: "pic.png",
          preview,
          status: "ready",
          attachment: picture,
        },
      ]}
      onRemove={() => {}}
      onInline={() => {}}
    />
  );
  let land!: (response: Response) => void;
  fetchMock.mockImplementation(
    () =>
      new Promise<Response>((resolve) => {
        land = resolve;
      }),
  );
  const view = mount();
  await view.render(row("blob:local-preview"));
  expect(view.thumbnails()).toEqual(["blob:local-preview"]);

  // The upload has landed: the preview is replaced by the stored URL, whose
  // bytes are still on their way. The thumbnail does not blank in between.
  await view.render(row("/v1/attachments/pic.png"));
  expect(view.thumbnails()).toEqual(["blob:local-preview"]);
  await act(async () => {
    land(new Response(new Uint8Array([1])));
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(view.thumbnails()).toEqual(["blob:image-1"]);
});

test("a result's picture waits for its box to come near the viewport before it asks", async () => {
  vi.stubGlobal("IntersectionObserver", FakeObserver);
  const view = mount();
  await view.render(
    <ResultImages
      seq={7}
      images={[{ media_type: "image/png", width: 1179, height: 2556 }]}
    />,
  );
  const img = view.host.querySelector<HTMLImageElement>("img.result-image")!;
  const box = view.host.querySelector("button.result-image-button");
  expect(fetchMock).not.toHaveBeenCalled();
  // Nothing is drawn, and no alt text stands in for it.
  expect(img.hasAttribute("src")).toBe(false);
  expect(img.getAttribute("alt")).toBe("");
  expect(img.getAttribute("height")).toBe("2556");

  // The box that holds the picture's place is what is watched, with a margin
  // past the viewport's top and bottom.
  expect(FakeObserver.all).toHaveLength(1);
  const [observer] = FakeObserver.all;
  expect([...observer.targets]).toEqual([box]);
  expect(observer.options?.rootMargin).toBe("1500px 0px");

  // Still far off: nothing is asked.
  await act(async () => {
    observer.report(false);
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(fetchMock).not.toHaveBeenCalled();

  // Near: it asks, once, and the watching is over.
  await act(async () => {
    observer.report(true);
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/log/7/images/0",
  ]);
  expect(img.getAttribute("src")).toBe("blob:image-1");
  expect(img.getAttribute("alt")).toBe("image/png 1179×2556");
  expect(observer.disconnected).toBe(true);
});

test("each result picture has its own gate", async () => {
  vi.stubGlobal("IntersectionObserver", FakeObserver);
  const view = mount();
  await view.render(
    <ResultImages
      seq={3}
      images={[
        { media_type: "image/png", width: 800, height: 600 },
        { media_type: "image/png", width: 800, height: 600 },
      ]}
    />,
  );
  expect(FakeObserver.all).toHaveLength(2);
  // The second comes near first, as it would scrolling up from below.
  await act(async () => {
    FakeObserver.all[1].report(true);
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/log/3/images/1",
  ]);
  await act(async () => {
    FakeObserver.all[0].report(true);
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/log/3/images/1",
    "/v1/log/3/images/0",
  ]);
});

test("attachment thumbnails ask when they mount, with no gate of their own", async () => {
  vi.stubGlobal("IntersectionObserver", FakeObserver);
  const view = mount();
  await view.render(<SentAttachments attachments={[picture]} />);
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/attachments/pic.png",
  ]);
  expect(FakeObserver.all).toEqual([]);
});

test("a result's picture is requested from the image route and opens the lightbox on that URL", async () => {
  const view = mount();
  await view.render(
    <>
      <ResultImages
        seq={7}
        images={[{ media_type: "image/png", width: 1179, height: 2556 }]}
      />
      <LightboxHost />
    </>,
  );
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    "/v1/log/7/images/0",
  ]);
  const img = view.host.querySelector<HTMLImageElement>("img.result-image")!;
  expect(img.getAttribute("src")).toBe("blob:image-1");
  // The aspect box and name stay on the picture however the bytes arrive.
  expect(img.getAttribute("width")).toBe("1179");
  expect(img.getAttribute("alt")).toBe("image/png 1179×2556");

  await act(async () => {
    view.host.querySelector<HTMLButtonElement>("button")!.click();
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(view.lightboxSrc()).toBe("blob:image-1");
  await view.unmount();
});
