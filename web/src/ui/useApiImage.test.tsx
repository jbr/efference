// @vitest-environment jsdom
// The API image cache (useApiImage.ts): one request per URL however many
// components hold it, the object URL kept until the last of them lets go, and
// nothing left behind afterwards.

import { StrictMode, act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { imageAttrs, useApiImage } from "./useApiImage.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

function Probe({ url }: { url: string | null }) {
  const src = useApiImage(url);
  return <img src={src ?? undefined} />;
}

let made = 0;
const createObjectURL = vi.fn<(bytes: Blob) => string>(
  () => `blob:image-${++made}`,
);
const revokeObjectURL = vi.fn();
const fetchMock = vi.fn<typeof fetch>();

const answer = () =>
  Promise.resolve(
    new Response(new Uint8Array([1, 2, 3]), {
      headers: { "content-type": "image/png" },
    }),
  );

beforeEach(() => {
  made = 0;
  createObjectURL.mockClear();
  revokeObjectURL.mockClear();
  fetchMock.mockReset();
  fetchMock.mockImplementation(answer);
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
  for (const unmount of mounted.splice(0)) unmount();
  vi.unstubAllGlobals();
});

function mount() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  mounted.push(() => act(() => root.unmount()));
  return {
    /** Render, then let the requests and the state updates they cause land. */
    render: (node: ReactNode) =>
      act(async () => {
        root.render(node);
        await new Promise((resolve) => setTimeout(resolve, 0));
      }),
    unmount: () => act(() => root.unmount()),
    srcs: () =>
      [...host.querySelectorAll("img")].map((img) => img.getAttribute("src")),
  };
}

describe("holding an API image", () => {
  it("shows the object URL over the fetched bytes", async () => {
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    expect(view.srcs()).toEqual(["blob:image-1"]);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/a");
    expect(init.signal).toBeInstanceOf(AbortSignal);
    const bytes = createObjectURL.mock.calls[0][0];
    expect(bytes.type).toBe("image/png");
    expect(bytes.size).toBe(3);
    await view.unmount();
  });

  it("has nothing to show until the bytes land", async () => {
    let land!: (response: Response) => void;
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          land = resolve;
        }),
    );
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    expect(view.srcs()).toEqual([null]);
    await act(async () => {
      land(await answer());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(view.srcs()).toEqual(["blob:image-1"]);
    await view.unmount();
  });

  it("shares one request among holders of the same URL", async () => {
    const view = mount();
    await view.render(
      <>
        <Probe url="/v1/a" />
        <Probe url="/v1/a" />
        <Probe url="/v1/b" />
      </>,
    );
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      "/v1/a",
      "/v1/b",
    ]);
    const [first, second, other] = view.srcs();
    expect(second).toBe(first);
    expect(other).not.toBe(first);
    await view.unmount();
  });

  it("shares a request that is still in flight", async () => {
    let land!: (response: Response) => void;
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          land = resolve;
        }),
    );
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    // A second holder arrives while the first request has not answered.
    await view.render(
      <>
        <Probe url="/v1/a" />
        <Probe url="/v1/a" />
      </>,
    );
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await act(async () => {
      land(await answer());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(view.srcs()).toEqual(["blob:image-1", "blob:image-1"]);
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    await view.unmount();
  });

  it("keeps the object URL until the last holder lets go, then revokes it", async () => {
    const view = mount();
    await view.render(
      <>
        <Probe url="/v1/a" />
        <Probe url="/v1/a" />
      </>,
    );
    await view.render(<Probe url="/v1/a" />);
    expect(revokeObjectURL).not.toHaveBeenCalled();
    expect(view.srcs()).toEqual(["blob:image-1"]);
    await view.unmount();
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
  });

  it("asks again when a holder arrives after the last one left", async () => {
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    await view.render(null);
    await view.render(<Probe url="/v1/a" />);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(view.srcs()).toEqual(["blob:image-2"]);
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
    await view.unmount();
  });

  it("shares the entry between a holder that leaves and one that arrives in the same commit", async () => {
    const view = mount();
    await view.render(<Probe key="echo" url="/v1/a" />);
    expect(view.srcs()).toEqual(["blob:image-1"]);
    // A different key: the first probe unmounts as the second mounts.
    await view.render(<Probe key="committed" url="/v1/a" />);
    expect(view.srcs()).toEqual(["blob:image-1"]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(revokeObjectURL).not.toHaveBeenCalled();
    await view.unmount();
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
  });

  it("draws bytes that have already landed on the first render", async () => {
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    // A server render runs no effects, so what it draws is what a holder
    // arriving now draws before its own effect has run.
    expect(renderToStaticMarkup(<Probe url="/v1/a" />)).toContain(
      '<img src="blob:image-1"/>',
    );
    expect(renderToStaticMarkup(<Probe url="/v1/b" />)).toBe("<img/>");
    await view.unmount();
    expect(renderToStaticMarkup(<Probe url="/v1/a" />)).toBe("<img/>");
  });

  it("moves to the new URL when the URL changes, releasing the old one", async () => {
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    await view.render(<Probe url="/v1/b" />);
    expect(view.srcs()).toEqual(["blob:image-2"]);
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
    await view.unmount();
    expect(revokeObjectURL).toHaveBeenCalledTimes(2);
  });

  it("does not show the old image again when the URL changes back before the new one lands", async () => {
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    let land!: (response: Response) => void;
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          land = resolve;
        }),
    );
    await view.render(<Probe url="/v1/b" />);
    // /v1/a's object URL is revoked; /v1/a is asked for afresh and has not
    // answered, so there is nothing to show — not the revoked URL.
    await view.render(<Probe url="/v1/a" />);
    expect(view.srcs()).toEqual([null]);
    await act(async () => {
      land(await answer());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(view.srcs()).toEqual(["blob:image-2"]);
    await view.unmount();
  });
});

describe("a request that does not produce an image", () => {
  it("shows nothing when the route refuses", async () => {
    fetchMock.mockImplementation(() =>
      Promise.resolve(new Response("{}", { status: 404 })),
    );
    const view = mount();
    await view.render(<Probe url="/v1/gone" />);
    expect(view.srcs()).toEqual([null]);
    expect(createObjectURL).not.toHaveBeenCalled();
    await view.unmount();
  });

  it("shows nothing when the request fails", async () => {
    fetchMock.mockImplementation(() =>
      Promise.reject(new TypeError("offline")),
    );
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    expect(view.srcs()).toEqual([null]);
    await view.unmount();
  });

  it("is asked again by the next holder rather than remembered", async () => {
    fetchMock.mockImplementationOnce(() =>
      Promise.resolve(new Response("{}", { status: 500 })),
    );
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    expect(view.srcs()).toEqual([null]);
    // The first holder is still mounted when the second arrives.
    await view.render(
      <>
        <Probe url="/v1/a" />
        <Probe url="/v1/a" />
      </>,
    );
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(view.srcs()).toEqual([null, "blob:image-1"]);
    await view.unmount();
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
  });

  it("aborts a request still in flight when the last holder leaves, and makes no object URL", async () => {
    let signal: AbortSignal | null | undefined;
    let land!: (response: Response) => void;
    fetchMock.mockImplementation((_url, init) => {
      signal = init?.signal;
      return new Promise<Response>((resolve) => {
        land = resolve;
      });
    });
    const view = mount();
    await view.render(<Probe url="/v1/a" />);
    expect(signal?.aborted).toBe(false);
    await view.render(null);
    expect(signal?.aborted).toBe(true);
    // An answer the abort did not stop still has nobody to show it to.
    await act(async () => {
      land(await answer());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(createObjectURL).not.toHaveBeenCalled();
    expect(revokeObjectURL).not.toHaveBeenCalled();
  });
});

describe("a URL that is not an API image", () => {
  it("makes no request for no URL", async () => {
    const view = mount();
    await view.render(<Probe url={null} />);
    expect(view.srcs()).toEqual([null]);
    expect(fetchMock).not.toHaveBeenCalled();
    await view.unmount();
  });

  it("passes a blob: URL through as given, with no request", async () => {
    const view = mount();
    await view.render(<Probe url="blob:local-preview" />);
    expect(view.srcs()).toEqual(["blob:local-preview"]);
    expect(fetchMock).not.toHaveBeenCalled();
    await view.unmount();
    expect(revokeObjectURL).not.toHaveBeenCalled();
  });

  it("moves from a local preview to the stored bytes without holding the preview", async () => {
    const view = mount();
    await view.render(<Probe url="blob:local-preview" />);
    await view.render(<Probe url="/v1/attachments/pic.png" />);
    expect(view.srcs()).toEqual(["blob:image-1"]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await view.unmount();
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
  });
});

describe("under StrictMode", () => {
  it("makes one request: the simulated unmount and remount share the entry", async () => {
    const view = mount();
    await view.render(
      <StrictMode>
        <Probe url="/v1/a" />
      </StrictMode>,
    );
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(view.srcs()).toEqual(["blob:image-1"]);
    expect(revokeObjectURL).not.toHaveBeenCalled();
    await view.unmount();
    expect(revokeObjectURL.mock.calls).toEqual([["blob:image-1"]]);
  });
});

describe("the attributes an <img> takes", () => {
  it("are the object URL and the alt text once there is a picture", () => {
    expect(imageAttrs("blob:image-1", "a chart")).toEqual({
      src: "blob:image-1",
      alt: "a chart",
    });
  });

  it("are no src and an empty alt while there is none, so no text is drawn in its place", () => {
    expect(imageAttrs(null, "a chart")).toEqual({ src: undefined, alt: "" });
  });
});
